//! File > Info > Inspect Document (#627): what the active document holds of
//! Word's inspector categories, and their Remove All. The page and the
//! harness's `inspect` verb both come here, so a test of these functions
//! tests the button.

use crate::{DocTab, Surface, live_comments};

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
    /// Whether any comment marker is in the body (one can outlive its comment).
    pub(crate) comment_markers: bool,
    /// Tracked changes, property changes and unsupported records included.
    pub(crate) revisions: usize,
    /// Hidden runs, tabs and breaks Remove All can remove.
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

/// Inspect a document tab; `None` for any other kind of tab.
pub(crate) fn inspect_doc_tab(tab: &DocTab) -> Option<Inspection> {
    let Surface::Doc(editor) = &tab.surface else {
        return None;
    };
    let doc = &editor.doc;
    Some(Inspection {
        // What the pane lists and a save writes: an undone new comment is
        // not there (#620).
        comments: live_comments(tab, doc).len(),
        comment_markers: docxcore::inspect::has_comment_markers(doc),
        revisions: doc.revisions().len(),
        hidden: docxcore::inspect::count_hidden_runs(doc),
        hidden_unremovable: docxcore::inspect::count_unremovable_hidden_runs(doc),
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
    let Surface::Doc(editor) = &mut tab.surface else {
        unreachable!("inspected above");
    };
    let (status, changed) = match category {
        InspectCategory::Comments => {
            let markers = editor.remove_all_comment_markers();
            // Every comment keeps its record, tracked: unlisted and unsaved
            // now its markers are gone, so undoing this brings each back
            // whole with them, and the save removes the rest from the base
            // per id (#620, #971).
            let tracked = &mut tab.tracked_comment_ids;
            let before = tracked.len();
            tracked.extend(tab.comments.iter().map(|c| c.id.clone()));
            let grew = tracked.len() > before;
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
            let removed = editor.remove_hidden_text();
            let left = docxcore::inspect::count_unremovable_hidden_runs(&editor.doc);
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
        tab.dirty = true;
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
                "bs-close"
            ]
        );
        // A workbook, a placeholder: no Info.
        assert!(!ids(false, false).contains(&"bs-info"));
        // A project: Export, no Info.
        let project = ids(true, false);
        assert!(!project.contains(&"bs-info"));
        assert!(project.contains(&"bs-export"));
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
}
