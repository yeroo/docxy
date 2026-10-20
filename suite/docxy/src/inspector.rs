//! File > Info > Inspect Document (#627): what the active document holds of
//! Word's inspector categories, and their Remove All. The page and the
//! harness's `inspect` verb both come here, so a test of these functions
//! tests the button.

use crate::{DocTab, Surface, live_comments};
use docxcore::inspect::{Stories, StoryCounts};
use docxcore::model::Document;
use docxcore::styles::StyleSheet;

/// One of the inspector's categories, in the order the page lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InspectCategory {
    Comments,
    Revisions,
    Hidden,
    Properties,
}

impl InspectCategory {
    pub(crate) const ALL: [InspectCategory; 4] = [
        InspectCategory::Comments,
        InspectCategory::Revisions,
        InspectCategory::Hidden,
        InspectCategory::Properties,
    ];

    /// The harness's name for it (`"remove": "<key>"`).
    pub(crate) fn key(self) -> &'static str {
        match self {
            InspectCategory::Comments => "comments",
            InspectCategory::Revisions => "revisions",
            InspectCategory::Hidden => "hidden",
            InspectCategory::Properties => "properties",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }

    /// The page's heading for it, as Word names it.
    pub(crate) fn title(self) -> &'static str {
        match self {
            InspectCategory::Comments => "Comments",
            InspectCategory::Revisions => "Revisions",
            InspectCategory::Hidden => "Hidden Text",
            InspectCategory::Properties => "Document Properties and Personal Information",
        }
    }

    /// The control id of its Remove All button.
    pub(crate) fn button_id(self) -> &'static str {
        match self {
            InspectCategory::Comments => "inspect-remove-comments",
            InspectCategory::Revisions => "inspect-remove-revisions",
            InspectCategory::Hidden => "inspect-remove-hidden",
            InspectCategory::Properties => "inspect-remove-properties",
        }
    }
}

/// Where the hidden runs Remove All leaves are.
const UNREMOVABLE_HIDDEN: &str = "inside tracked moves, fields, shapes or other preserved XML";

/// What a document tab holds, per category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Inspection {
    /// Comments in the tab's comment list.
    pub(crate) comments: usize,
    /// Whether any comment marker is in the body, a header, a footer or a
    /// note (one can outlive its comment).
    pub(crate) comment_markers: bool,
    /// Tracked changes, property changes and unsupported records included.
    pub(crate) revisions: usize,
    /// Hidden runs, tabs and breaks Remove All can remove, in the body,
    /// headers, footers and notes, hidden directly or through their styles.
    pub(crate) hidden: usize,
    /// Hidden runs left in raw XML (tracked moves, fields, a group shape's
    /// other text boxes, other preserved XML such as `w:customXml` or an
    /// unmodeled block): found and counted, but Remove All leaves them.
    pub(crate) hidden_unremovable: usize,
    pub(crate) properties: bool,
}

impl Inspection {
    pub(crate) fn found(&self, category: InspectCategory) -> bool {
        match category {
            InspectCategory::Comments => self.comments > 0 || self.comment_markers,
            InspectCategory::Revisions => self.revisions > 0,
            InspectCategory::Hidden => self.hidden + self.hidden_unremovable > 0,
            InspectCategory::Properties => self.properties,
        }
    }

    /// The count the page and the harness report; properties have none.
    pub(crate) fn count(&self, category: InspectCategory) -> Option<usize> {
        match category {
            InspectCategory::Comments => Some(self.comments),
            InspectCategory::Revisions => Some(self.revisions),
            InspectCategory::Hidden => Some(self.hidden + self.hidden_unremovable),
            InspectCategory::Properties => None,
        }
    }

    /// The page's result line for `category`.
    pub(crate) fn line(&self, category: InspectCategory) -> String {
        let n = self.count(category).unwrap_or(0);
        let s = if n == 1 { "" } else { "s" };
        match (category, self.found(category)) {
            // Markers whose comments are gone: nothing to count, still found.
            (InspectCategory::Comments, true) if n == 0 => "Comment markers were found.".into(),
            (InspectCategory::Comments, true) => format!("{n} comment{s} found."),
            (InspectCategory::Comments, false) => "No comments were found.".into(),
            (InspectCategory::Revisions, true) => format!("{n} revision{s} found."),
            (InspectCategory::Revisions, false) => "No revisions were found.".into(),
            (InspectCategory::Hidden, true) if self.hidden_unremovable > 0 => format!(
                "{n} hidden run{s} found ({} {UNREMOVABLE_HIDDEN} cannot be removed).",
                self.hidden_unremovable
            ),
            (InspectCategory::Hidden, true) => format!("{n} hidden run{s} found."),
            (InspectCategory::Hidden, false) => "No hidden text was found.".into(),
            (InspectCategory::Properties, true) => {
                "Document properties and personal information were found.".into()
            }
            (InspectCategory::Properties, false) => {
                "No document properties or personal information were found.".into()
            }
        }
    }
}

/// The line the Info page shows under its rows for the last Remove All
/// (`status`, with the tab index it ran on), while `active` is still that tab.
pub(crate) fn status_line(
    status: Option<&(usize, Result<String, String>)>,
    active: usize,
) -> Option<String> {
    let (_, result) = status.filter(|(tab, _)| *tab == active)?;
    Some(match result {
        Ok(text) => text.clone(),
        Err(e) => format!("Could not remove: {e}"),
    })
}

/// The tab's styles, which hidden text is resolved through; `None` for a
/// document without a package (only direct `w:vanish` hides there).
fn tab_styles(tab: &DocTab) -> Option<StyleSheet> {
    let xml = tab.pkg.as_ref()?.part_text("word/styles.xml")?;
    Some(docxcore::styles::parse_styles_xml(&xml))
}

/// What the headers, footers and notes of the tab, whose body is `doc`,
/// hold (#918). A header or footer open in its editor is counted from the
/// editor, which may not have been written back to the package yet.
fn story_counts(tab: &DocTab, doc: &Document, styles: Option<&StyleSheet>) -> StoryCounts {
    let Some(pkg) = tab.pkg.as_ref() else {
        return StoryCounts::default();
    };
    let editing = tab.hf_edit.as_ref();
    let mut counts =
        Stories::new(pkg, doc).count(pkg, styles, editing.map(|h| h.part_name.as_str()));
    if let Some(hf) = editing {
        counts += docxcore::inspect::count_story_blocks(&hf.editor.doc.body, styles);
    }
    counts
}

/// Inspect a document tab; `None` for any other kind of tab.
pub(crate) fn inspect_doc_tab(tab: &DocTab) -> Option<Inspection> {
    let Surface::Doc(editor) = &tab.surface else {
        return None;
    };
    let doc = &editor.doc;
    let styles = tab_styles(tab);
    let stories = story_counts(tab, doc, styles.as_ref());
    Some(Inspection {
        // What the pane lists and a save writes: an undone new comment is
        // not there (#620).
        comments: live_comments(tab, doc).len(),
        comment_markers: docxcore::inspect::has_comment_markers(doc) || stories.comment_markers > 0,
        revisions: doc.revisions().len(),
        hidden: docxcore::inspect::count_hidden_runs(doc, styles.as_ref()) + stories.hidden,
        hidden_unremovable: docxcore::inspect::count_unremovable_hidden_runs(doc)
            + stories.hidden_unremovable,
        properties: tab
            .pkg
            .as_ref()
            .is_some_and(docxcore::inspect::has_personal_properties),
    })
}

/// Remove All for `category`: the status line it leaves on the tab, or why
/// it can't run. Removing something marks the tab dirty; a category that
/// was not found changes nothing but that status line ("… nothing to
/// remove"). Revisions are accepted, never deleted with their text.
///
/// Comments and hidden text go from the headers, footers and notes too
/// (#918). Those are package edits, not on the editor's undo stack (like
/// Remove All properties): an undo brings back what the body lost only,
/// except that the comments' story markers return in the saved file once an
/// undo anchors those comments again (#981). An open header/footer editor
/// is written back first, and reopened on its area when its part changed.
pub(crate) fn inspect_remove(
    tab: &mut DocTab,
    category: InspectCategory,
) -> Result<String, String> {
    let inspection = inspect_doc_tab(tab).ok_or("the active tab is not a document")?;
    let found = inspection.found(category);
    if !found {
        let status = format!("{}: nothing to remove", category.title());
        tab.status = status.clone().into();
        return Ok(status);
    }
    let in_stories = matches!(
        category,
        InspectCategory::Comments | InspectCategory::Hidden
    );
    let hf_open = tab
        .hf_edit
        .as_ref()
        .filter(|_| in_stories)
        .map(|h| (h.part_name.clone(), h.section, h.is_header, h.variant));
    if hf_open.is_some() {
        crate::flush_hf_tab(tab);
    }
    let part_bytes = |tab: &DocTab| {
        let (part, ..) = hf_open.as_ref()?;
        tab.pkg.as_ref()?.part(part).map(<[u8]>::to_vec)
    };
    let edited_before = part_bytes(tab);
    let styles = tab_styles(tab);
    let Surface::Doc(editor) = &mut tab.surface else {
        unreachable!("inspected above");
    };
    let stories = tab
        .pkg
        .as_ref()
        .filter(|_| in_stories)
        .map(|pkg| Stories::new(pkg, &editor.doc));
    let (status, changed) = match category {
        InspectCategory::Comments => {
            // What the headers, footers and notes lose: the markers of
            // comments no undo can bring back (header-only, orphans) are
            // gone for good; those anchored in the body are kept, so the
            // save can put them back while an undo has the comments
            // anchored again. A comment deleted earlier is neither: its
            // Delete Comment step is still on the undo stack, and the save
            // leaves its markers out while it is not live (#981).
            let anchored = docxcore::inspect::comment_marker_ids(&editor.doc);
            let mut stripped = 0;
            if let Some((pkg, stories)) = tab.pkg.as_mut().zip(stories.as_ref()) {
                let present = stories.comment_marker_ids(pkg);
                let tracked = &tab.tracked_comment_ids;
                let kept: Vec<&str> = present
                    .iter()
                    .map(String::as_str)
                    .filter(|id| anchored.contains(*id))
                    .collect();
                let gone: Vec<&str> = present
                    .iter()
                    .map(String::as_str)
                    .filter(|id| !anchored.contains(*id) && !tracked.contains(*id))
                    .collect();
                stripped = tab
                    .comment_undo
                    .strip_stories(pkg, stories, &gone, &kept, None);
            }
            let markers = editor.remove_all_comment_markers() + stripped;
            for c in &tab.comments {
                tab.comment_undo.keep_raw(tab.pkg.as_ref(), &c.id);
            }
            // Every comment keeps its record, tracked: unlisted and unsaved
            // now its markers are gone, so undoing this brings each back
            // whole with them, and the save removes the rest from the base
            // per id (#620, #971).
            let tracked = &mut tab.tracked_comment_ids;
            let before = tracked.len();
            tracked.extend(tab.comments.iter().map(|c| c.id.clone()));
            let grew = tracked.len() > before;
            // And the save drops the base comments it can't match per id.
            tab.comments_removed_all = true;
            let comments = inspection.comments;
            (
                format!("Removed all comments ({comments})"),
                markers > 0 || grew,
            )
        }
        InspectCategory::Revisions => {
            let outcomes = editor.accept_all_revisions();
            let accepted = outcomes.iter().filter(|o| o.is_applied()).count();
            let failed = outcomes.len() - accepted;
            let status = if failed == 0 {
                format!("Accepted all revisions ({accepted})")
            } else {
                format!(
                    "Accepted {accepted} revision{}; {failed} revision{} could not be accepted",
                    if accepted == 1 { "" } else { "s" },
                    if failed == 1 { "" } else { "s" },
                )
            };
            (status, accepted > 0)
        }
        InspectCategory::Hidden => {
            let styles = styles.as_ref();
            let removed = editor.remove_hidden_text(styles)
                + tab
                    .pkg
                    .as_mut()
                    .zip(stories.as_ref())
                    .map_or(0, |(pkg, stories)| stories.remove_hidden_text(pkg, styles));
            let left = docxcore::inspect::count_unremovable_hidden_runs(&editor.doc)
                + tab
                    .pkg
                    .as_ref()
                    .zip(stories.as_ref())
                    .map_or(0, |(pkg, stories)| {
                        stories.count(pkg, styles, None).hidden_unremovable
                    });
            let status = if left == 0 {
                format!("Removed all hidden text ({removed})")
            } else {
                format!(
                    "Removed {removed} hidden run{}; {left} {UNREMOVABLE_HIDDEN} could not be removed",
                    if removed == 1 { "" } else { "s" },
                )
            };
            (status, removed > 0)
        }
        InspectCategory::Properties => {
            let changed = tab
                .pkg
                .as_mut()
                .is_some_and(docxcore::inspect::remove_personal_properties);
            (
                "Removed document properties and personal information".to_string(),
                changed,
            )
        }
    };
    if changed {
        tab.set_dirty();
    }
    // The open editor shows its part as it was: reopen it on the same area
    // (its caret goes to the start).
    if let Some((_, section, is_header, variant)) = hf_open
        && part_bytes(tab) != edited_before
    {
        crate::hf_tab::reopen(tab, section, is_header, variant);
    }
    tab.status = status.clone().into();
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, save_doc_tab, tab_from_path};
    use std::path::{Path, PathBuf};

    /// A fresh directory under the target dir. Not `std::env::temp_dir()`:
    /// see `session_and_hot_both_follow_the_override`.
    fn temp(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/inspect-tests")
            .join(format!("{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

    /// A .docx holding one of everything the inspector finds: two comments
    /// (one with its author before its id), an insertion and a deletion (a
    /// comment marker and a hidden run inside the insertion), hidden text,
    /// and core/app properties.
    fn inspected_docx() -> Vec<u8> {
        let document = format!(
            "<w:document {W}><w:body>\
             <w:p><w:commentRangeStart w:id=\"0\"/><w:r><w:t>Visible</w:t></w:r>\
             <w:commentRangeEnd w:id=\"0\"/><w:r><w:commentReference w:id=\"0\"/></w:r>\
             <w:r><w:rPr><w:vanish/></w:rPr><w:t>Secret</w:t></w:r></w:p>\
             <w:p><w:ins w:id=\"5\" w:author=\"A\" w:date=\"2026-01-01T00:00:00Z\">\
             <w:r><w:t>Added</w:t></w:r><w:commentRangeStart w:id=\"1\"/>\
             <w:r><w:rPr><w:vanish/></w:rPr><w:t>Buried</w:t></w:r><w:commentRangeEnd w:id=\"1\"/>\
             <w:r><w:commentReference w:id=\"1\"/></w:r></w:ins>\
             <w:del w:id=\"6\" w:author=\"A\" w:date=\"2026-01-01T00:00:00Z\">\
             <w:r><w:delText>Dropped</w:delText></w:r></w:del></w:p>\
             </w:body></w:document>"
        );
        let comments = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <w:comments {W}>\
             <w:comment w:id=\"0\" w:author=\"Ann\" w:initials=\"A\"><w:p><w:r><w:t>one</w:t></w:r></w:p></w:comment>\
             <w:comment w:author=\"Bob\" w:id=\"1\" w:initials=\"B\"><w:p><w:r><w:t>two</w:t></w:r></w:p></w:comment>\
             </w:comments>"
        );
        let core = "<?xml version=\"1.0\"?><cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\" xmlns:dcterms=\"t\">\
            <dc:creator>Ann</dc:creator><cp:lastModifiedBy>Bob</cp:lastModifiedBy>\
            <dcterms:created>2026-01-01T00:00:00Z</dcterms:created></cp:coreProperties>";
        let app = "<Properties xmlns=\"p\"><Company>Acme</Company></Properties>";
        let rels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
        let parts: Vec<(String, Vec<u8>)> = [
            ("[Content_Types].xml", "<?xml version=\"1.0\"?><Types/>"),
            ("_rels/.rels", rels),
            ("word/document.xml", &document),
            ("word/styles.xml", "<?xml version=\"1.0\"?><w:styles/>"),
            ("word/comments.xml", &comments),
            ("docProps/core.xml", core),
            ("docProps/app.xml", app),
        ]
        .into_iter()
        .map(|(n, x)| (n.to_string(), x.as_bytes().to_vec()))
        .collect();
        docxcore::zipwrite::write_zip(&parts)
    }

    fn open(tag: &str) -> (PathBuf, DocTab) {
        let dir = temp(tag);
        let path = dir.join("inspected.docx");
        std::fs::write(&path, inspected_docx()).unwrap();
        let tab = tab_from_path(&path);
        assert!(matches!(tab.kind, Kind::Docx));
        assert!(!tab.load_failed, "{}", tab.status);
        (path, tab)
    }

    fn body_text(tab: &DocTab) -> String {
        let Surface::Doc(ed) = &tab.surface else {
            panic!("not a document")
        };
        ed.doc.body.iter().map(|b| b.plain_text()).collect()
    }

    fn part(path: &Path, name: &str) -> String {
        let pkg = docxcore::package::load_package(&std::fs::read(path).unwrap()).unwrap();
        pkg.part_text(name).unwrap_or_default()
    }

    #[test]
    fn inspect_doc_tab_reports_all_four_categories() {
        let (_, tab) = open("report");
        let found = inspect_doc_tab(&tab).unwrap();
        assert_eq!(found.comments, 2);
        assert!(found.comment_markers);
        assert_eq!(found.revisions, 2);
        assert_eq!(found.hidden, 2);
        assert!(found.properties);
        for category in InspectCategory::ALL {
            assert!(found.found(category), "{category:?}");
        }
        assert_eq!(found.line(InspectCategory::Comments), "2 comments found.");
    }

    /// Review r2 m1: markers with no comment left are found, not "0 comments".
    #[test]
    fn orphan_comment_markers_have_their_own_line() {
        let orphans = Inspection {
            comments: 0,
            comment_markers: true,
            revisions: 0,
            hidden: 0,
            hidden_unremovable: 0,
            properties: false,
        };
        assert!(orphans.found(InspectCategory::Comments));
        assert_eq!(
            orphans.line(InspectCategory::Comments),
            "Comment markers were found."
        );
        let none = Inspection {
            comment_markers: false,
            ..orphans
        };
        assert_eq!(
            none.line(InspectCategory::Comments),
            "No comments were found."
        );
    }

    #[test]
    fn inspect_is_none_for_other_tabs() {
        let mut tab = crate::sheet_tab_from_path(&PathBuf::from("missing.xlsx"), false);
        assert!(inspect_doc_tab(&tab).is_none());
        assert!(inspect_remove(&mut tab, InspectCategory::Comments).is_err());
    }

    #[test]
    fn inspect_remove_revisions_accepts_not_deletes() {
        let (_, mut tab) = open("revisions");
        let status = inspect_remove(&mut tab, InspectCategory::Revisions).unwrap();
        assert_eq!(status, "Accepted all revisions (2)");
        assert!(tab.dirty);
        let text = body_text(&tab);
        assert!(text.contains("Added"), "inserted text kept: {text}");
        assert!(!text.contains("Dropped"), "deleted text gone: {text}");
        assert!(
            !inspect_doc_tab(&tab)
                .unwrap()
                .found(InspectCategory::Revisions)
        );
    }

    #[test]
    fn inspect_remove_noop_leaves_tab_clean() {
        let (_, mut tab) = open("noop");
        inspect_remove(&mut tab, InspectCategory::Hidden).unwrap();
        tab.dirty = false;
        let status = inspect_remove(&mut tab, InspectCategory::Hidden).unwrap();
        assert_eq!(status, "Hidden Text: nothing to remove");
        assert_eq!(
            tab.status.as_ref(),
            status,
            "the tab's status line says so too"
        );
        assert!(!tab.dirty);
        let Surface::Doc(ed) = &mut tab.surface else {
            unreachable!()
        };
        // The first removal's undo step is the only one.
        assert!(ed.undo());
        assert!(!ed.undo());
    }

    #[test]
    fn inspect_remove_all_then_save_round_trips() {
        let (path, mut tab) = open("round-trip");
        for category in InspectCategory::ALL {
            inspect_remove(&mut tab, category).unwrap();
            let after = inspect_doc_tab(&tab).unwrap();
            assert!(!after.found(category), "{category:?}: {after:?}");
        }
        assert!(tab.dirty);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);

        let comments = part(&path, "word/comments.xml");
        assert!(!comments.contains("<w:comment "), "{comments}");
        let document = part(&path, "word/document.xml");
        for gone in [
            "commentRangeStart",
            "commentRangeEnd",
            "commentReference",
            "<w:ins ",
            "<w:del ",
            "<w:vanish",
            "Secret",
            "Buried",
            "Dropped",
        ] {
            assert!(!document.contains(gone), "{gone}: {document}");
        }
        for kept in ["Visible", "Added"] {
            assert!(document.contains(kept), "{kept}: {document}");
        }
        let core = part(&path, "docProps/core.xml");
        assert!(!core.contains("dc:creator"), "{core}");
        assert!(!core.contains("cp:lastModifiedBy"), "{core}");
        assert!(core.contains("dcterms:created"), "{core}");
        assert!(!part(&path, "docProps/app.xml").contains("Company"));

        // Reopened, the document has nothing left to inspect.
        let reopened = tab_from_path(&path);
        let found = inspect_doc_tab(&reopened).unwrap();
        for category in InspectCategory::ALL {
            assert!(!found.found(category), "{category:?}: {found:?}");
        }
    }

    #[test]
    fn backstage_rail_info_only_for_documents() {
        let ids = |project, doc| {
            crate::backstage_rail_items(project, doc)
                .map(|item| item.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(false, true),
            [
                "bs-back",
                "bs-info",
                "bs-new",
                "bs-open",
                "bs-save",
                "bs-saveas",
                "bs-export-doc",
                "bs-account",
                "bs-close"
            ]
        );
        // A workbook, a placeholder: no Info, but every tab kind has Account (#1023).
        assert!(!ids(false, false).contains(&"bs-info"));
        for (project, doc) in [(false, false), (true, false), (false, true)] {
            assert!(ids(project, doc).contains(&"bs-account"), "{project} {doc}");
        }
        // A project: Export, no Info.
        let project = ids(true, false);
        assert!(!project.contains(&"bs-info"));
        assert!(project.contains(&"bs-export"));
        // A document's Export page is its own (#635); a project's Export…
        // exports the Gantt chart.
        assert!(!project.contains(&"bs-export-doc"));
        assert!(!ids(false, true).contains(&"bs-export"));
        assert!(!ids(false, false).contains(&"bs-export-doc"));
    }

    /// Review r1 m3: the Info page shows the last Remove All's result, an
    /// error included, only on the tab it ran on.
    #[test]
    fn status_line_shows_the_result_on_its_own_tab() {
        let ok = (
            2,
            Ok("Accepted 1 revision; 1 revision could not be accepted".to_string()),
        );
        assert_eq!(
            status_line(Some(&ok), 2).as_deref(),
            Some("Accepted 1 revision; 1 revision could not be accepted")
        );
        assert_eq!(status_line(Some(&ok), 0), None);
        assert_eq!(status_line(None, 2), None);
        let err = (0, Err("the active tab is not a document".to_string()));
        assert_eq!(
            status_line(Some(&err), 0).as_deref(),
            Some("Could not remove: the active tab is not a document")
        );
    }

    /// Review r3 M2: hidden runs Remove All can't reach still count as
    /// found, and both the line and the status say how many stay.
    #[test]
    fn hidden_runs_left_in_raw_xml_are_reported() {
        let dir = temp("raw-hidden");
        let path = dir.join("raw-hidden.docx");
        let hidden = "<w:r><w:rPr><w:vanish/></w:rPr><w:t>Secret</w:t></w:r>";
        let document = format!(
            "<w:document {W}><w:body>\
             <w:p><w:moveTo w:id=\"3\" w:author=\"A\">{hidden}</w:moveTo></w:p>\
             <w:p><w:fldSimple w:instr=\" XE &quot;term&quot; \">{hidden}</w:fldSimple></w:p>\
             <w:p>{hidden}<w:r><w:t>Visible</w:t></w:r></w:p>\
             </w:body></w:document>"
        );
        let rels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
        let parts: Vec<(String, Vec<u8>)> = [
            ("[Content_Types].xml", "<?xml version=\"1.0\"?><Types/>"),
            ("_rels/.rels", rels),
            ("word/document.xml", &document),
        ]
        .into_iter()
        .map(|(n, x)| (n.to_string(), x.as_bytes().to_vec()))
        .collect();
        std::fs::write(&path, docxcore::zipwrite::write_zip(&parts)).unwrap();
        let mut tab = tab_from_path(&path);

        let found = inspect_doc_tab(&tab).unwrap();
        assert_eq!((found.hidden, found.hidden_unremovable), (1, 2));
        assert_eq!(found.count(InspectCategory::Hidden), Some(3));
        assert_eq!(
            found.line(InspectCategory::Hidden),
            "3 hidden runs found (2 inside tracked moves, fields, shapes or other preserved XML cannot be removed)."
        );
        let status = inspect_remove(&mut tab, InspectCategory::Hidden).unwrap();
        assert_eq!(
            status,
            "Removed 1 hidden run; 2 inside tracked moves, fields, shapes or other preserved XML could not be removed"
        );
        assert!(tab.dirty);
        let after = inspect_doc_tab(&tab).unwrap();
        assert!(
            after.found(InspectCategory::Hidden),
            "the raw ones stay found"
        );
        assert_eq!((after.hidden, after.hidden_unremovable), (0, 2));
    }

    #[test]
    fn inspect_category_keys_round_trip() {
        for category in InspectCategory::ALL {
            assert_eq!(InspectCategory::from_key(category.key()), Some(category));
        }
        assert_eq!(InspectCategory::from_key("macros"), None);
    }

    const W14: &str = "xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\"";
    const R: &str =
        "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";

    /// Write `parts` (with a package relationship to the document) as a .docx
    /// under a fresh directory and open it.
    fn open_parts(tag: &str, parts: &[(&str, String)]) -> (PathBuf, DocTab) {
        let path = temp(tag).join(format!("{tag}.docx"));
        let rels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
        let mut zip = vec![("_rels/.rels".to_string(), rels.as_bytes().to_vec())];
        zip.extend(
            parts
                .iter()
                .map(|(n, x)| (n.to_string(), x.as_bytes().to_vec())),
        );
        std::fs::write(&path, docxcore::zipwrite::write_zip(&zip)).unwrap();
        let tab = tab_from_path(&path);
        assert!(!tab.load_failed, "{}", tab.status);
        (path, tab)
    }

    /// `[Content_Types].xml` with an override per named part.
    fn content_types(parts: &[&str]) -> String {
        let overrides: String = parts
            .iter()
            .map(|p| format!("<Override PartName=\"/{p}\" ContentType=\"application/xml\"/>"))
            .collect();
        format!(
            "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             {overrides}</Types>"
        )
    }

    /// `word/_rels/document.xml.rels` relating each `(id, type, target)`.
    fn document_rels(rels: &[(&str, &str, &str)]) -> String {
        let rels: String = rels
            .iter()
            .map(|(id, ty, target)| {
                format!(
                    "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/{ty}\" Target=\"{target}\"/>"
                )
            })
            .collect();
        format!(
            "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>"
        )
    }

    /// A document with a comment and a style-hidden run in its body, a header
    /// holding a hidden run and the only markers of comment 3, and a footnote
    /// holding a hidden run and the only markers of comment 4.
    fn storied_parts() -> Vec<(&'static str, String)> {
        let document = format!(
            "<w:document {W} {R}><w:body>\
             <w:p><w:commentRangeStart w:id=\"0\"/><w:r><w:t>Visible</w:t></w:r><w:commentRangeEnd w:id=\"0\"/>\
             <w:r><w:commentReference w:id=\"0\"/></w:r>\
             <w:r><w:rPr><w:rStyle w:val=\"Secret\"/></w:rPr><w:t>Styled</w:t></w:r>\
             <w:r><w:footnoteReference w:id=\"1\"/></w:r></w:p>\
             <w:sectPr><w:headerReference w:type=\"default\" r:id=\"rIdH\"/></w:sectPr>\
             </w:body></w:document>"
        );
        let header = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:hdr {W} {R}><w:p><w:commentRangeStart w:id=\"3\"/><w:r><w:t>Head</w:t></w:r>\
             <w:commentRangeEnd w:id=\"3\"/><w:r><w:commentReference w:id=\"3\"/></w:r>\
             <w:r><w:rPr><w:vanish/></w:rPr><w:t>Hush</w:t></w:r></w:p></w:hdr>"
        );
        let footnotes = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:footnotes {W}>\
             <w:footnote w:type=\"separator\" w:id=\"-1\"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>\
             <w:footnote w:type=\"continuationSeparator\" w:id=\"0\"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>\
             <w:footnote w:id=\"1\"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space=\"preserve\"> Note</w:t></w:r>\
             <w:r><w:rPr><w:vanish/></w:rPr><w:t>Quiet</w:t></w:r>\
             <w:commentRangeStart w:id=\"4\"/><w:commentRangeEnd w:id=\"4\"/><w:r><w:commentReference w:id=\"4\"/></w:r></w:p></w:footnote>\
             </w:footnotes>"
        );
        let comment = |id: &str, text: &str| {
            format!(
                "<w:comment w:id=\"{id}\" w:author=\"Ann\" w:initials=\"A\"><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:comment>"
            )
        };
        let comments = format!(
            "<?xml version=\"1.0\"?><w:comments {W}>{}{}{}</w:comments>",
            comment("0", "body"),
            comment("3", "head"),
            comment("4", "note")
        );
        let styles = format!(
            "<?xml version=\"1.0\"?><w:styles {W}>\
             <w:style w:type=\"character\" w:styleId=\"Secret\"><w:rPr><w:vanish/></w:rPr></w:style></w:styles>"
        );
        vec![
            (
                "[Content_Types].xml",
                content_types(&[
                    "word/document.xml",
                    "word/header1.xml",
                    "word/footnotes.xml",
                    "word/comments.xml",
                    "word/styles.xml",
                ]),
            ),
            (
                "word/_rels/document.xml.rels",
                document_rels(&[
                    ("rIdH", "header", "header1.xml"),
                    ("rIdF", "footnotes", "footnotes.xml"),
                    ("rIdC", "comments", "comments.xml"),
                    ("rIdS", "styles", "styles.xml"),
                ]),
            ),
            ("word/document.xml", document),
            ("word/header1.xml", header),
            ("word/footnotes.xml", footnotes),
            ("word/comments.xml", comments),
            ("word/styles.xml", styles),
        ]
    }

    /// #918 p1: Inspect Document finds hidden text and comment markers in the
    /// header and the footnote, and hidden text through a character style;
    /// Remove All takes them all, and the save keeps them gone: no comment
    /// whose markers were only in the header or the note is left either.
    #[test]
    fn remove_all_in_headers_and_notes_survives_save() {
        let (path, mut tab) = open_parts("stories", &storied_parts());
        let found = inspect_doc_tab(&tab).unwrap();
        assert_eq!(found.comments, 3);
        assert!(found.comment_markers);
        assert_eq!(
            (found.hidden, found.hidden_unremovable),
            (3, 0),
            "{found:?}"
        );
        assert_eq!(found.line(InspectCategory::Hidden), "3 hidden runs found.");
        let footnotes_before = part(&path, "word/footnotes.xml");

        assert_eq!(
            inspect_remove(&mut tab, InspectCategory::Hidden).unwrap(),
            "Removed all hidden text (3)"
        );
        assert_eq!(
            inspect_remove(&mut tab, InspectCategory::Comments).unwrap(),
            "Removed all comments (3)"
        );
        let after = inspect_doc_tab(&tab).unwrap();
        for category in [InspectCategory::Hidden, InspectCategory::Comments] {
            assert!(!after.found(category), "{category:?}: {after:?}");
        }
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);

        let header = part(&path, "word/header1.xml");
        for gone in ["w:vanish", "Hush", "commentR"] {
            assert!(!header.contains(gone), "{gone}: {header}");
        }
        assert!(header.contains("Head"), "{header}");
        let footnotes = part(&path, "word/footnotes.xml");
        for gone in ["w:vanish", "Quiet", "commentR"] {
            assert!(!footnotes.contains(gone), "{gone}: {footnotes}");
        }
        for kept in ["<w:footnoteRef/>", " Note", "<w:footnote w:id=\"1\">"] {
            assert!(footnotes.contains(kept), "{kept}: {footnotes}");
        }
        // The separators are as they were.
        let separators = |xml: &str| xml[..xml.find("<w:footnote w:id=\"1\"").unwrap()].to_string();
        assert_eq!(separators(&footnotes), separators(&footnotes_before));
        let document = part(&path, "word/document.xml");
        assert!(!document.contains("Styled"), "{document}");
        assert!(
            document.contains("<w:footnoteReference w:id=\"1\"/>"),
            "{document}"
        );
        assert!(document.contains("Visible"), "{document}");
        let comments = part(&path, "word/comments.xml");
        assert!(!comments.contains("<w:comment "), "{comments}");

        let reopened = tab_from_path(&path);
        let found = inspect_doc_tab(&reopened).unwrap();
        for category in [InspectCategory::Hidden, InspectCategory::Comments] {
            assert!(!found.found(category), "{category:?}: {found:?}");
        }
    }

    /// #918 p1: comment markers whose only place is a header are found, even
    /// with no comment left to count.
    #[test]
    fn comment_markers_only_in_a_header_are_found() {
        let mut parts = storied_parts();
        parts.retain(|(name, _)| *name != "word/comments.xml");
        for (name, xml) in parts.iter_mut() {
            if *name == "word/document.xml" {
                *xml = format!(
                    "<w:document {W} {R}><w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p>\
                     <w:sectPr><w:headerReference w:type=\"default\" r:id=\"rIdH\"/></w:sectPr></w:body></w:document>"
                );
            }
        }
        parts.retain(|(name, _)| *name != "word/footnotes.xml");
        let (path, mut tab) = open_parts("header-markers", &parts);
        let found = inspect_doc_tab(&tab).unwrap();
        assert_eq!(found.comments, 0);
        assert!(found.comment_markers);
        assert_eq!(
            found.line(InspectCategory::Comments),
            "Comment markers were found."
        );
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        assert!(tab.dirty, "a package edit dirties the tab");
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let header = part(&path, "word/header1.xml");
        assert!(!header.contains("commentR"), "{header}");
        assert!(header.contains("w:vanish"), "hidden text stays: {header}");
    }

    /// #918 p1: with the header open in its editor, Inspect counts what the
    /// editor holds, and Remove All writes it back, removes from it and
    /// reopens the editor on the result.
    #[test]
    fn remove_all_with_the_header_editor_open() {
        let (path, mut tab) = open_parts("hf-open", &storied_parts());
        assert!(crate::hf::open(
            &mut tab,
            0,
            true,
            docxcore::package::HeaderVariant::Default
        ));
        let part_name = tab.hf_edit.as_ref().unwrap().part_name.clone();
        assert_eq!(part_name, "word/header1.xml");
        // A hidden run typed in the header editor, not yet written back.
        let hf = tab.hf_edit.as_mut().unwrap();
        hf.editor.doc.body.push(docxcore::model::Block::Paragraph(
            docxcore::model::Paragraph {
                content: vec![docxcore::model::Inline::Run(docxcore::model::Run {
                    text: "Typed".into(),
                    props: docxcore::model::RunProps {
                        vanish: true,
                        ..Default::default()
                    },
                })],
                ..Default::default()
            },
        ));
        let found = inspect_doc_tab(&tab).unwrap();
        assert_eq!(found.hidden, 4, "{found:?}");
        inspect_remove(&mut tab, InspectCategory::Hidden).unwrap();
        let hf = tab.hf_edit.as_ref().expect("the editor stays open");
        let text: String = hf.editor.doc.body.iter().map(|b| b.plain_text()).collect();
        assert_eq!(text, "Head");
        assert_eq!(inspect_doc_tab(&tab).unwrap().hidden, 0);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let header = part(&path, "word/header1.xml");
        assert!(
            !header.contains("Typed") && !header.contains("Hush"),
            "{header}"
        );
    }

    /// Comments 1 (Ann) and 2 (Bob) with every part Word writes beside
    /// comments.xml, and a tracked insertion by Rev.
    fn extras_parts() -> Vec<(&'static str, String)> {
        let document = format!(
            "<w:document {W}><w:body>\
             <w:p><w:commentRangeStart w:id=\"1\"/><w:r><w:t>One</w:t></w:r><w:commentRangeEnd w:id=\"1\"/>\
             <w:r><w:commentReference w:id=\"1\"/></w:r></w:p>\
             <w:p><w:commentRangeStart w:id=\"2\"/><w:r><w:t>Two</w:t></w:r><w:commentRangeEnd w:id=\"2\"/>\
             <w:r><w:commentReference w:id=\"2\"/></w:r>\
             <w:ins w:id=\"9\" w:author=\"Rev\" w:date=\"2026-01-01T00:00:00Z\"><w:r><w:t>New</w:t></w:r></w:ins></w:p>\
             </w:body></w:document>"
        );
        let comments = format!(
            "<?xml version=\"1.0\"?><w:comments {W} {W14}>\
             <w:comment w:id=\"1\" w:author=\"Ann\" w:initials=\"A\"><w:p w14:paraId=\"0000AAAA\"><w:r><w:t>one</w:t></w:r></w:p></w:comment>\
             <w:comment w:id=\"2\" w:author=\"Bob\" w:initials=\"B\"><w:p w14:paraId=\"0000BBBB\"><w:r><w:t>two</w:t></w:r></w:p></w:comment>\
             </w:comments>"
        );
        let extended = "<?xml version=\"1.0\"?><w15:commentsEx xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\">\
            <w15:commentEx w15:paraId=\"0000AAAA\" w15:done=\"0\"/><w15:commentEx w15:paraId=\"0000BBBB\" w15:done=\"0\"/></w15:commentsEx>";
        let ids = "<?xml version=\"1.0\"?><w16cid:commentsIds xmlns:w16cid=\"http://schemas.microsoft.com/office/word/2016/wordml/cid\">\
            <w16cid:commentId w16cid:paraId=\"0000AAAA\" w16cid:durableId=\"0A0A0A0A\"/>\
            <w16cid:commentId w16cid:paraId=\"0000BBBB\" w16cid:durableId=\"0B0B0B0B\"/></w16cid:commentsIds>";
        let extensible = "<?xml version=\"1.0\"?><w16cex:commentsExtensible xmlns:w16cex=\"http://schemas.microsoft.com/office/word/2018/wordml/cex\">\
            <w16cex:commentExtensible w16cex:durableId=\"0A0A0A0A\" w16cex:dateUtc=\"2026-01-01T00:00:00Z\"/>\
            <w16cex:commentExtensible w16cex:durableId=\"0B0B0B0B\" w16cex:dateUtc=\"2026-01-02T00:00:00Z\"/></w16cex:commentsExtensible>";
        let people = "<?xml version=\"1.0\"?><w15:people xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\">\
            <w15:person w15:author=\"Ann\"><w15:presenceInfo w15:providerId=\"None\" w15:userId=\"Ann\"/></w15:person>\
            <w15:person w15:author=\"Bob\"><w15:presenceInfo w15:providerId=\"None\" w15:userId=\"Bob\"/></w15:person>\
            <w15:person w15:author=\"Rev\"><w15:presenceInfo w15:providerId=\"None\" w15:userId=\"Rev\"/></w15:person>\
            </w15:people>";
        let names = [
            "word/document.xml",
            "word/comments.xml",
            "word/commentsExtended.xml",
            "word/commentsIds.xml",
            "word/commentsExtensible.xml",
            "word/people.xml",
        ];
        vec![
            ("[Content_Types].xml", content_types(&names)),
            (
                "word/_rels/document.xml.rels",
                document_rels(&[
                    ("rId2", "comments", "comments.xml"),
                    ("rId3", "commentsExtended", "commentsExtended.xml"),
                    ("rId4", "commentsIds", "commentsIds.xml"),
                    ("rId5", "commentsExtensible", "commentsExtensible.xml"),
                    ("rId6", "people", "people.xml"),
                ]),
            ),
            ("word/document.xml", document),
            ("word/comments.xml", comments),
            ("word/commentsExtended.xml", extended.to_string()),
            ("word/commentsIds.xml", ids.to_string()),
            ("word/commentsExtensible.xml", extensible.to_string()),
            ("word/people.xml", people.to_string()),
        ]
    }

    /// #918 p2: after Remove All comments and Save, no comment part is left
    /// and people.xml names only someone something still names: the
    /// revision's author.
    #[test]
    fn remove_all_comments_leaves_no_stale_extras_or_people_after_save() {
        let (path, mut tab) = open_parts("extras", &extras_parts());
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let pkg = docxcore::package::load_package(&std::fs::read(&path).unwrap()).unwrap();
        for gone in [
            "word/comments.xml",
            "word/commentsExtended.xml",
            "word/commentsIds.xml",
            "word/commentsExtensible.xml",
        ] {
            assert!(pkg.part(gone).is_none(), "{gone}");
        }
        let people = pkg.part_text("word/people.xml").unwrap();
        assert!(
            !people.contains("\"Ann\"") && !people.contains("\"Bob\""),
            "{people}"
        );
        assert!(people.contains("w15:author=\"Rev\""), "{people}");
    }

    /// #918 FIX r1 m2: a comment added after Remove All keeps its author's
    /// person, though the save writes it after the base dropped the others.
    #[test]
    fn a_comment_added_after_remove_all_keeps_its_person() {
        let (path, mut tab) = open_parts("extras-readd", &extras_parts());
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        let Surface::Doc(ed) = &mut tab.surface else {
            unreachable!()
        };
        ed.select_all();
        let added = crate::add_doc_comment(
            &mut tab,
            "again".to_string(),
            ("Bob".to_string(), "B".to_string()),
        );
        assert!(added.is_some());
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let comments = part(&path, "word/comments.xml");
        assert!(comments.contains("w:author=\"Bob\""), "{comments}");
        let people = part(&path, "word/people.xml");
        assert!(people.contains("w15:author=\"Bob\""), "{people}");
        assert!(!people.contains("\"Ann\""), "{people}");
    }

    /// #918 p2: deleting one of two comments drops its entries, the
    /// commentsExtensible one included, and its author's person; the other
    /// comment keeps everything.
    #[test]
    fn delete_comment_drops_its_extensible_entry_and_person() {
        let (path, mut tab) = open_parts("extras-one", &extras_parts());
        crate::delete_doc_comment(&mut tab, "1");
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let extensible = part(&path, "word/commentsExtensible.xml");
        assert!(!extensible.contains("0A0A0A0A"), "{extensible}");
        assert!(extensible.contains("0B0B0B0B"), "{extensible}");
        let ids = part(&path, "word/commentsIds.xml");
        assert!(
            !ids.contains("0A0A0A0A") && ids.contains("0B0B0B0B"),
            "{ids}"
        );
        let people = part(&path, "word/people.xml");
        assert!(!people.contains("\"Ann\""), "{people}");
        assert!(
            people.contains("\"Bob\"") && people.contains("\"Rev\""),
            "{people}"
        );
    }

    /// Every `w:commentReference` id in `document` has its comment in
    /// `comments`, and these are the comment ids, in order.
    fn assert_no_dangling(document: &str, comments: &str) -> Vec<String> {
        let ids: Vec<String> = docxcore::comments::parse_comments_xml(comments)
            .into_iter()
            .map(|c| c.id)
            .collect();
        for id in docxcore::inspect::comment_marker_ids_in_xml(document) {
            assert!(ids.contains(&id), "{id} dangles: {comments}");
        }
        ids
    }

    fn undo(tab: &mut DocTab) {
        let Surface::Doc(ed) = &mut tab.surface else {
            unreachable!()
        };
        assert!(ed.undo());
    }

    /// #918 p5: undo of Remove All comments brings the comments back with
    /// their markers, and the save writes every one.
    #[test]
    fn undo_remove_all_comments_saves_every_comment() {
        let (path, mut tab) = open("undo-remove-all");
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        undo(&mut tab);
        assert_eq!(inspect_doc_tab(&tab).unwrap().comments, 2);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let document = part(&path, "word/document.xml");
        let comments = part(&path, "word/comments.xml");
        assert_eq!(assert_no_dangling(&document, &comments), ["0", "1"]);
        assert!(
            comments.contains(">one<") && comments.contains(">two<"),
            "{comments}"
        );
    }

    /// #918 p5: Remove All, Save, Undo, Save: the first save left the
    /// comments out of the file the tab now rests on, and the second writes
    /// them back from their records, with no marker left dangling.
    #[test]
    fn remove_all_save_undo_save_restores_every_comment() {
        let (path, mut tab) = open("remove-save-undo");
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert!(!part(&path, "word/comments.xml").contains("<w:comment "));
        undo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let document = part(&path, "word/document.xml");
        let comments = part(&path, "word/comments.xml");
        assert_eq!(assert_no_dangling(&document, &comments), ["0", "1"]);
        assert!(
            comments.contains(">one<") && comments.contains(">two<"),
            "{comments}"
        );
    }

    /// #918 p5: the same for a single Delete Comment and its undo.
    #[test]
    fn undo_delete_comment_saves_the_comment() {
        let (path, mut tab) = open("undo-delete");
        crate::delete_doc_comment(&mut tab, "1");
        assert_eq!(inspect_doc_tab(&tab).unwrap().comments, 1);
        undo(&mut tab);
        assert_eq!(inspect_doc_tab(&tab).unwrap().comments, 2);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let document = part(&path, "word/document.xml");
        let comments = part(&path, "word/comments.xml");
        assert_eq!(assert_no_dangling(&document, &comments), ["0", "1"]);
    }

    /// [`storied_parts`] with comment 3 also anchored in the body, as Word
    /// writes a comment its author later extended into a header.
    fn shared_parts() -> Vec<(&'static str, String)> {
        let mut parts = storied_parts();
        for (name, xml) in &mut parts {
            if *name == "word/document.xml" {
                *xml = xml.replace(
                    "<w:r><w:commentReference w:id=\"0\"/></w:r>",
                    "<w:r><w:commentReference w:id=\"0\"/></w:r>\
                     <w:commentRangeStart w:id=\"3\"/><w:commentRangeEnd w:id=\"3\"/>\
                     <w:r><w:commentReference w:id=\"3\"/></w:r>",
                );
            }
        }
        parts
    }

    fn redo(tab: &mut DocTab) {
        let Surface::Doc(ed) = &mut tab.surface else {
            unreachable!()
        };
        assert!(ed.redo());
    }

    fn saved_ids(path: &Path, name: &str) -> usize {
        part(path, name).matches("w:id=\"3\"").count()
    }

    /// #981 plan-suite-hf: Delete Comment on a comment also anchored in a
    /// header takes the header's markers from the file with the body's; its
    /// undo brings them and the comment back, its redo takes them again.
    #[test]
    fn delete_comment_takes_its_header_markers_and_undo_restores_them() {
        let (path, mut tab) = open_parts("hf-delete", &shared_parts());
        assert_eq!(inspect_doc_tab(&tab).unwrap().comments, 3);
        crate::delete_doc_comment(&mut tab, "3");
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert_eq!(saved_ids(&path, "word/header1.xml"), 0);
        assert_eq!(saved_ids(&path, "word/document.xml"), 0);
        assert!(!part(&path, "word/comments.xml").contains(">head<"));
        assert!(
            part(&path, "word/header1.xml").contains("Head"),
            "the header text stays"
        );

        undo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert_eq!(saved_ids(&path, "word/header1.xml"), 3);
        assert_eq!(saved_ids(&path, "word/document.xml"), 3);
        assert!(part(&path, "word/comments.xml").contains(">head<"));

        redo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert_eq!(saved_ids(&path, "word/header1.xml"), 0);
        assert!(!part(&path, "word/comments.xml").contains(">head<"));
    }

    /// #981: Remove All takes each header's and note's markers once, and an
    /// undo puts them back with the body's.
    #[test]
    fn remove_all_then_undo_puts_the_header_and_note_markers_back() {
        let (path, mut tab) = open_parts("hf-remove-all", &shared_parts());
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert!(!part(&path, "word/header1.xml").contains("commentR"));
        assert!(!part(&path, "word/footnotes.xml").contains("commentR"));
        undo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert_eq!(saved_ids(&path, "word/header1.xml"), 3);
        assert_eq!(saved_ids(&path, "word/document.xml"), 3);
        let comments = part(&path, "word/comments.xml");
        assert!(
            comments.contains(">head<") && comments.contains(">body<"),
            "{comments}"
        );
        redo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert!(!part(&path, "word/header1.xml").contains("commentR"));
    }

    /// #981: a loaded comment anchored only in a header is never hidden or
    /// dropped by what happens to the others, and a new comment never takes
    /// the id of a marker that only a header holds.
    #[test]
    fn a_header_only_comment_stays_and_its_id_is_not_reused() {
        let (path, mut tab) = open_parts("hf-only", &storied_parts());
        crate::delete_doc_comment(&mut tab, "0");
        let ids: Vec<String> = match &tab.surface {
            Surface::Doc(ed) => crate::live_comments(&tab, &ed.doc),
            _ => unreachable!(),
        }
        .into_iter()
        .map(|c| c.id)
        .collect();
        assert_eq!(ids, ["3", "4"]);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert_eq!(saved_ids(&path, "word/header1.xml"), 3);
        let comments = part(&path, "word/comments.xml");
        assert!(
            comments.contains(">head<") && comments.contains(">note<"),
            "{comments}"
        );
        // 3 and 4 are marked only in the header and the note, with no
        // comment of theirs anywhere: the next id is still 5.
        tab.comments.clear();
        tab.used_comment_ids.clear();
        tab.tracked_comment_ids.clear();
        let pkg = tab.pkg.as_mut().unwrap();
        pkg.remove_comment_id("3");
        pkg.remove_comment_id("4");
        let Surface::Doc(ed) = &mut tab.surface else {
            unreachable!()
        };
        ed.select_all();
        let id = crate::add_doc_comment(&mut tab, "new".into(), ("Jo".into(), "J".into()));
        assert_eq!(id, Some(5));
    }

    /// #981 FIX r1: a converted tab saved while a comment is deleted rebases
    /// on a package without the header's markers; the undo still brings them
    /// back with the comment.
    #[test]
    fn converted_tab_delete_save_undo_save_restores_the_header_markers() {
        let (path, mut tab) = open_parts("hf-converted", &shared_parts());
        tab.access.converted = Some(crate::open_mode::Converted::Rtf);
        crate::delete_doc_comment(&mut tab, "3");
        let path = path.with_file_name("converted.docx");
        assert!(save_doc_tab(&mut tab, Some(path.clone())), "{}", tab.status);
        assert_eq!(saved_ids(&path, "word/header1.xml"), 0);
        assert!(tab.access.converted.is_none(), "now that Word document");
        undo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert_eq!(saved_ids(&path, "word/header1.xml"), 3);
        assert_eq!(saved_ids(&path, "word/document.xml"), 3);
        assert!(part(&path, "word/comments.xml").contains(">head<"));
    }

    /// [`shared_parts`] with comment 3 deleted first, and a second header
    /// comment (7) anchored in the header only.
    fn header_with_shared_and_orphan_parts() -> Vec<(&'static str, String)> {
        let mut parts = shared_parts();
        for (name, xml) in &mut parts {
            if *name == "word/header1.xml" {
                *xml = xml.replace(
                    "<w:r><w:rPr><w:vanish/>",
                    "<w:commentRangeStart w:id=\"7\"/><w:commentRangeEnd w:id=\"7\"/>\
                     <w:r><w:commentReference w:id=\"7\"/></w:r><w:r><w:rPr><w:vanish/>",
                );
            }
        }
        parts
    }

    /// #981 FIX r2: Remove All with a header holding a body-anchored comment
    /// (3) and a header-only one (7): the undo brings 3's header markers
    /// back, 7's stay gone.
    #[test]
    fn remove_all_with_a_header_only_comment_still_restores_the_other_markers() {
        let (path, mut tab) = open_parts("hf-mixed", &header_with_shared_and_orphan_parts());
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        undo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        assert_eq!(saved_ids(&path, "word/header1.xml"), 3);
        assert_eq!(saved_ids(&path, "word/document.xml"), 3);
        assert!(!part(&path, "word/header1.xml").contains("w:id=\"7\""));
    }

    /// #981 FIX r2: Delete Comment 3, then Remove All, then one undo: comment
    /// 1's markers in the header are back (comment 3 stays deleted).
    #[test]
    fn delete_comment_then_remove_all_then_undo_restores_the_other_header_markers() {
        let mut parts = shared_parts();
        for (name, xml) in &mut parts {
            // Comment 0 anchored in the header too.
            if *name == "word/header1.xml" {
                *xml = xml.replace(
                    "<w:r><w:rPr><w:vanish/>",
                    "<w:commentRangeStart w:id=\"0\"/><w:commentRangeEnd w:id=\"0\"/>\
                     <w:r><w:commentReference w:id=\"0\"/></w:r><w:r><w:rPr><w:vanish/>",
                );
            }
        }
        let (path, mut tab) = open_parts("hf-delete-then-all", &parts);
        crate::delete_doc_comment(&mut tab, "3");
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        undo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let header = part(&path, "word/header1.xml");
        assert_eq!(header.matches("w:id=\"0\"").count(), 3, "{header}");
        assert!(!header.contains("w:id=\"3\""), "{header}");
        assert!(!part(&path, "word/comments.xml").contains(">head<"));
    }

    /// [`shared_parts`] with comment 0 anchored in the header too.
    fn parts_with_zero_in_the_header() -> Vec<(&'static str, String)> {
        let mut parts = shared_parts();
        for (name, xml) in &mut parts {
            if *name == "word/header1.xml" {
                *xml = xml.replace(
                    "<w:r><w:rPr><w:vanish/>",
                    "<w:commentRangeStart w:id=\"0\"/><w:commentRangeEnd w:id=\"0\"/>\
                     <w:r><w:commentReference w:id=\"0\"/></w:r><w:r><w:rPr><w:vanish/>",
                );
            }
        }
        parts
    }

    /// #981 FIX r3: Delete Comment 3, Remove All, then both undone: the
    /// header has both comments' markers again.
    #[test]
    fn delete_remove_all_then_both_undone_restores_the_deleted_ones_markers_too() {
        let (path, mut tab) = open_parts("hf-delete-all-undo2", &parts_with_zero_in_the_header());
        crate::delete_doc_comment(&mut tab, "3");
        inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
        undo(&mut tab);
        undo(&mut tab);
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let header = part(&path, "word/header1.xml");
        for id in ["0", "3"] {
            assert_eq!(
                header.matches(&format!("w:id=\"{id}\"")).count(),
                3,
                "{header}"
            );
        }
        assert_eq!(saved_ids(&path, "word/document.xml"), 3);
    }

    /// #981 FIX r3: on a converted tab, a header-only comment deleted after
    /// a body-anchored one leaves the first one's record matching its part.
    #[test]
    fn converted_tab_deleting_a_header_only_comment_keeps_the_other_restorable() {
        let mut parts = parts_with_zero_in_the_header();
        for (name, xml) in &mut parts {
            if *name == "word/header1.xml" {
                *xml = xml.replace(
                    "<w:r><w:rPr><w:vanish/>",
                    "<w:commentRangeStart w:id=\"7\"/><w:commentRangeEnd w:id=\"7\"/>\
                     <w:r><w:commentReference w:id=\"7\"/></w:r><w:r><w:rPr><w:vanish/>",
                );
            }
        }
        let (path, mut tab) = open_parts("hf-converted-two", &parts);
        tab.access.converted = Some(crate::open_mode::Converted::Rtf);
        crate::delete_doc_comment(&mut tab, "0");
        crate::delete_doc_comment(&mut tab, "7");
        undo(&mut tab);
        let path = path.with_file_name("converted2.docx");
        assert!(save_doc_tab(&mut tab, Some(path.clone())), "{}", tab.status);
        let header = part(&path, "word/header1.xml");
        assert_eq!(header.matches("w:id=\"0\"").count(), 3, "{header}");
        assert!(!header.contains("w:id=\"7\""), "{header}");
    }
}
