//! File > Info's Properties (#1364): Title, Tags (`cp:keywords`) and Author
//! (`dc:creator`) of a document, edited in place. Each is a one-line field on
//! `field_edit` like the Home Font box (#1361): a click focuses it with the
//! whole value selected; Enter, Tab, a click elsewhere or leaving Info applies
//! it as one undo step (`Editor::set_core_field`), Escape restores it. The
//! values live in the editor (undo carries them); `DocTab::set_dirty` writes
//! what differs from `docProps/core.xml`.

use docxcore::editor::CoreFields;
use docxcore::package::Package;
use docxcore::sdt::BoundProp;
use gpui::{Context, KeyDownEvent, Window};

use crate::{Docxy, Surface, close, field_edit};

/// The part the three properties live in.
const CORE_PART: &str = "docProps/core.xml";

/// An Info property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InfoProp {
    Title,
    Tags,
    Author,
}

impl InfoProp {
    pub(crate) const ALL: [InfoProp; 3] = [Self::Title, Self::Tags, Self::Author];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Tags => "Tags",
            Self::Author => "Author",
        }
    }

    /// The `pointer-click {"input":…}` id.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Title => "info-title",
            Self::Tags => "info-tags",
            Self::Author => "info-author",
        }
    }

    /// The probe the field is drawn under.
    pub(crate) fn probe(self) -> &'static str {
        match self {
            Self::Title => "info-prop:title",
            Self::Tags => "info-prop:tags",
            Self::Author => "info-prop:author",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    fn bound(self) -> BoundProp {
        match self {
            Self::Title => BoundProp::Title,
            Self::Tags => BoundProp::Keywords,
            Self::Author => BoundProp::Author,
        }
    }

    fn step(self, forward: bool) -> Self {
        let at = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        let n = Self::ALL.len();
        Self::ALL[(at + if forward { 1 } else { n - 1 }) % n]
    }
}

/// The focused property field, tied to the tab it was opened on.
pub(crate) struct InfoEdit {
    pub(crate) which: InfoProp,
    pub(crate) text: String,
    pub(crate) caret: usize,
    pub(crate) sel: bool,
    pub(crate) tab: close::TabId,
}

/// The file's Title, Tags and Author.
pub(crate) fn core_fields_of(pkg: &Package) -> CoreFields {
    let props = docxcore::field::parse_core_props(&pkg.part_text(CORE_PART).unwrap_or_default());
    CoreFields {
        title: props.title,
        tags: props.keywords,
        author: props.author,
    }
}

impl crate::DocTab {
    /// Give the editor the file's core properties if it has none yet (an
    /// editor made without them), with no undo step.
    pub(crate) fn seed_core_fields(&mut self) {
        if let (Surface::Doc(ed), Some(pkg)) = (&mut self.surface, &self.pkg) {
            if !ed.core_seeded() {
                ed.reseed_core_fields(core_fields_of(pkg));
            }
        }
    }

    /// Write what an edit, undo or redo changed in Title, Tags and Author to
    /// the package (adding the core properties part a file lacks), and show
    /// it in the open header or footer's controls.
    pub(crate) fn sync_core_fields(&mut self) {
        self.seed_core_fields();
        let (Surface::Doc(ed), Some(pkg)) = (&mut self.surface, self.pkg.as_mut()) else {
            return;
        };
        let unsynced = ed.core_unsynced();
        if unsynced.is_empty() {
            return;
        }
        let step = ed.undo_serial();
        pkg.ensure_core_properties();
        for (prop, text) in &unsynced {
            let written = pkg
                .part_text(prop.part_name())
                .and_then(|xml| prop.write(&xml, text));
            if let Some(new) = written {
                pkg.set_part_text(prop.part_name(), &new);
            }
            if let Some(h) = self.hf_edit.as_mut() {
                h.editor.set_bound_property(
                    *prop,
                    text,
                    docxcore::editor::PropSource::Other { newest_step: step },
                );
            }
        }
        ed.core_synced();
    }

    /// Whether the tab will save as a Word package, so a property edit is
    /// kept: not Markdown, Plain Text, Rich Text or an editable-HTML page
    /// (`save_doc_tab` writes those without the package).
    fn saves_as_word(&self) -> bool {
        !self.markdown
            && self.bundle_html.is_none()
            && self.path.as_ref().is_none_or(|p| {
                matches!(
                    crate::html_bundle::doc_target(p, crate::is_markdown_path(p)),
                    crate::html_bundle::DocTarget::Docx(_)
                )
            })
    }

    /// Whether the file has, or can be given, the part the properties are
    /// written to. A document with no package yet (a new one, or a file read
    /// by hand) is given the one a save would make, as Mailings does
    /// (`mailings_tab::package`), when it saves as a Word package; the part
    /// is added to a package that lacks it. False for a tab that cannot hold
    /// properties and for a package that cannot register the part.
    pub(crate) fn ensure_core_part(&mut self) -> bool {
        if self.pkg.is_none() {
            let Surface::Doc(ed) = &self.surface else {
                return false;
            };
            if !self.saves_as_word() {
                return false;
            }
            let bytes = crate::doc_to_docx(&ed.doc, &crate::live_comments(self, &ed.doc), None);
            match docxcore::package::load_package(&bytes) {
                Ok(pkg) => self.pkg = Some(pkg),
                Err(_) => return false,
            }
        }
        self.seed_core_fields();
        self.pkg
            .as_mut()
            .is_some_and(|p| p.ensure_core_properties())
    }
}

impl Docxy {
    /// What property `which` holds in the active document.
    pub(crate) fn info_prop_value(&self, which: InfoProp) -> String {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => ed
                .core_fields()
                .get(which.bound())
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        }
    }

    /// Whether a property field has the keyboard: File > Info up on the tab
    /// the field was opened on.
    pub(crate) fn info_edit_live(&self) -> bool {
        let Some(e) = &self.info_edit else {
            return false;
        };
        self.backstage
            && self.bs_info
            && self.active_is_doc()
            && self.active_tab_id().as_ref() == Some(&e.tab)
    }

    /// Focus property field `which` with its value selected (a click). A
    /// document in Protected View or marked as final shows them read-only.
    pub(crate) fn info_edit_open(&mut self, which: InfoProp, cx: &mut Context<Self>) {
        if !self.bs_info || !self.active_is_doc() {
            return;
        }
        if self.info_edit_live() && self.info_edit.as_ref().is_some_and(|e| e.which == which) {
            return;
        }
        // Apply the field being left first: its refusal note must outlive the
        // clearing below.
        self.info_note = None;
        self.info_edit_apply(cx);
        if self.active_locked() {
            self.info_note = self.active_tab_id().map(|tab| {
                (
                    tab,
                    format!(
                        "{} is read-only: this document is in Protected View or marked as final",
                        which.label()
                    ),
                )
            });
            cx.notify();
            return;
        }
        let Some(tab) = self.active_tab_id() else {
            return;
        };
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.seed_core_fields();
        }
        let text = self.info_prop_value(which);
        let caret = text.chars().count();
        self.info_edit = Some(InfoEdit {
            which,
            sel: caret > 0,
            caret,
            text,
            tab,
        });
        cx.notify();
    }

    /// Apply the focused field to its tab and close it (Enter, Tab, a click
    /// elsewhere, leaving Info, Save, closing). An unchanged value records no
    /// step; a file without a core properties part is given one, and a
    /// package that cannot hold one refuses with a note.
    pub(crate) fn info_edit_apply(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.info_edit.take() else {
            return;
        };
        cx.notify();
        let Some(idx) = self
            .tabs
            .iter()
            .position(|t| (t.title.clone(), t.path.clone()) == e.tab)
        else {
            return;
        };
        let tab = &mut self.tabs[idx];
        tab.seed_core_fields();
        let same = matches!(&tab.surface, Surface::Doc(ed)
            if ed.core_fields().get(e.which.bound()) == Some(e.text.as_str()));
        if same {
            return;
        }
        if !tab.ensure_core_part() {
            self.info_note = Some((
                e.tab.clone(),
                format!(
                    "Could not change {}: this file cannot hold document properties",
                    e.which.label()
                ),
            ));
            return;
        }
        let Surface::Doc(ed) = &mut tab.surface else {
            return;
        };
        if ed.set_core_field(e.which.bound(), &e.text) {
            tab.mark_dirty();
            self.info_note = None;
        }
    }

    /// Tab (`forward`) or Shift+Tab applies the focused field and focuses the
    /// next or previous one; they arrive as `tab_key` / `shift_tab_key`, not
    /// as keys.
    pub(crate) fn info_edit_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(e) = &self.info_edit {
            let next = e.which.step(forward);
            self.info_edit_open(next, cx);
        }
    }

    /// Route a keystroke to the focused property field, as the Font box's.
    pub(crate) fn info_edit_key(
        &mut self,
        ev: &KeyDownEvent,
        ctrl: bool,
        alt: bool,
        key: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use field_edit::Op;
        let Some(e) = self.info_edit.as_mut() else {
            return;
        };
        let edit = |e: &mut InfoEdit, op: Op| {
            field_edit::apply(&mut e.text, &mut e.caret, &mut e.sel, op);
        };
        // Ctrl+Alt is AltGr on Windows: its keys are text, not chords.
        if ctrl && !alt {
            match key {
                "a" => edit(e, Op::SelectAll),
                "c" | "x" => {
                    let text = e.text.clone();
                    if e.sel && !text.is_empty() {
                        if key == "x" {
                            edit(e, Op::Delete);
                        }
                        self.clipboard_write(text.clone(), cx);
                        self.office_clip.push(&text);
                    }
                }
                "v" => {
                    // A one-line field: line breaks and controls drop out.
                    if let crate::ClipRead::Text(text) = self.clipboard_read(cx) {
                        let line = field_edit::one_line(&text);
                        if !line.is_empty() {
                            if let Some(e) = self.info_edit.as_mut() {
                                edit(e, Op::Insert(&line));
                            }
                        }
                    }
                }
                "enter" => self.info_edit_apply(cx),
                // Any other chord is not text.
                _ => {}
            }
            cx.notify();
            return;
        }
        match key {
            "escape" => self.info_edit = None,
            "enter" => self.info_edit_apply(cx),
            "backspace" => edit(e, Op::Backspace),
            "delete" => edit(e, Op::Delete),
            "left" => edit(e, Op::Left),
            "right" => edit(e, Op::Right),
            "home" => edit(e, Op::Home),
            "end" => edit(e, Op::End),
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        edit(e, Op::Insert(c));
                    }
                }
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocTab, Kind, save_doc_tab, tab_from_path};
    use docxcore::sdt::BoundProp;
    use std::path::{Path, PathBuf};

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("docxy-info-props-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A .docx whose core part holds a Title, Tags and an Author, or none.
    fn docx(core: Option<&str>) -> Vec<u8> {
        let document = format!(
            "<w:document {W}><w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p></w:body></w:document>"
        );
        let rels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
        let mut parts: Vec<(&str, &str)> = vec![
            ("[Content_Types].xml", "<?xml version=\"1.0\"?><Types xmlns=\"t\"></Types>"),
            ("_rels/.rels", rels),
            ("word/document.xml", &document),
            ("word/styles.xml", "<?xml version=\"1.0\"?><w:styles/>"),
        ];
        if let Some(core) = core {
            parts.push(("docProps/core.xml", core));
        }
        let parts: Vec<(String, Vec<u8>)> = parts
            .into_iter()
            .map(|(n, x)| (n.to_string(), x.as_bytes().to_vec()))
            .collect();
        docxcore::zipwrite::write_zip(&parts)
    }

    const CORE: &str = "<?xml version=\"1.0\"?><cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\">\
        <dc:title>Old</dc:title><cp:keywords>x, y</cp:keywords><dc:creator>Ann</dc:creator>\
        </cp:coreProperties>";

    fn open(tag: &str, core: Option<&str>) -> (PathBuf, DocTab) {
        let dir = temp(tag);
        let path = dir.join("props.docx");
        std::fs::write(&path, docx(core)).unwrap();
        let tab = tab_from_path(&path);
        assert!(matches!(tab.kind, Kind::Docx));
        assert!(!tab.load_failed, "{}", tab.status);
        (path, tab)
    }

    fn editor(tab: &mut DocTab) -> &mut docxcore::editor::Editor {
        let Surface::Doc(ed) = &mut tab.surface else {
            panic!("not a document")
        };
        ed
    }

    fn core_xml(tab: &DocTab) -> String {
        tab.pkg.as_ref().unwrap().part_text(CORE_PART).unwrap()
    }

    fn part(path: &Path, name: &str) -> String {
        let pkg = docxcore::package::load_package(&std::fs::read(path).unwrap()).unwrap();
        pkg.part_text(name).unwrap_or_default()
    }

    #[test]
    fn the_editor_opens_with_the_files_title_tags_and_author() {
        let (_, mut tab) = open("seed", Some(CORE));
        let f = editor(&mut tab).core_fields().clone();
        assert_eq!(
            (f.title.as_str(), f.tags.as_str(), f.author.as_str()),
            ("Old", "x, y", "Ann")
        );
        let (_, mut bare) = open("seed-none", None);
        assert_eq!(editor(&mut bare).core_fields(), &CoreFields::default());
    }

    /// A file with no core properties part (most files other tools make)
    /// gets one on the first edit, and a reopen shows the value.
    #[test]
    fn a_file_without_the_part_is_given_one_1364() {
        let (path, mut tab) = open("bare", None);
        assert!(tab.pkg.as_ref().unwrap().part_text(CORE_PART).is_none());
        assert!(tab.ensure_core_part());
        assert!(editor(&mut tab).set_core_field(BoundProp::Keywords, "q & r"));
        tab.mark_dirty();
        assert!(core_xml(&tab).contains("<cp:keywords>q &amp; r</cp:keywords>"));
        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let mut again = tab_from_path(&path);
        assert_eq!(editor(&mut again).core_fields().tags, "q & r");
        // An edit that never asked for the part (undo of a value the file
        // already held) adds none.
        let (_, mut idle) = open("bare-idle", None);
        idle.set_dirty();
        assert!(idle.pkg.as_ref().unwrap().part_text(CORE_PART).is_none());
    }

    /// An edit reaches core.xml (escaped) through `set_dirty`, so do undo and
    /// redo, and a save then a reopen shows it.
    #[test]
    fn an_edit_undo_and_redo_reach_core_xml_and_a_reopen_shows_it() {
        let (path, mut tab) = open("edit", Some(CORE));
        assert!(editor(&mut tab).set_core_field(BoundProp::Title, "A & <B>"));
        assert!(editor(&mut tab).set_core_field(BoundProp::Author, "Bo"));
        tab.mark_dirty();
        assert!(tab.dirty);
        let xml = core_xml(&tab);
        assert!(xml.contains("<dc:title>A &amp; &lt;B&gt;</dc:title>"), "{xml}");
        assert!(xml.contains("<dc:creator>Bo</dc:creator>"), "{xml}");
        assert!(xml.contains("<cp:keywords>x, y</cp:keywords>"), "{xml}");

        assert!(editor(&mut tab).undo());
        tab.set_dirty();
        let xml = core_xml(&tab);
        assert!(xml.contains("<dc:creator>Ann</dc:creator>"), "{xml}");
        assert!(xml.contains("<dc:title>A &amp; &lt;B&gt;</dc:title>"), "{xml}");
        assert!(editor(&mut tab).redo());
        tab.set_dirty();
        assert!(core_xml(&tab).contains("<dc:creator>Bo</dc:creator>"));

        assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
        let saved = part(&path, CORE_PART);
        assert!(saved.contains("<dc:title>A &amp; &lt;B&gt;</dc:title>"), "{saved}");
        let mut again = tab_from_path(&path);
        let f = editor(&mut again).core_fields().clone();
        assert_eq!((f.title.as_str(), f.author.as_str()), ("A & <B>", "Bo"));
    }

    /// An unchanged value records no step; so does one Info does not edit.
    #[test]
    fn an_unchanged_value_records_no_step() {
        let (_, mut tab) = open("same", Some(CORE));
        let ed = editor(&mut tab);
        let before = ed.undo_serial();
        assert!(!ed.set_core_field(BoundProp::Title, "Old"));
        assert_eq!(ed.undo_serial(), before);
        assert!(!tab.dirty);
    }

    /// Inspect Document's Remove All Properties takes the values out of the
    /// file; no later sync or undo may write them back.
    #[test]
    fn remove_all_properties_is_not_undone_by_the_editor() {
        let (_, mut tab) = open("remove", Some(CORE));
        assert!(editor(&mut tab).set_core_field(BoundProp::Author, "Bo"));
        tab.mark_dirty();
        crate::inspector::inspect_remove(&mut tab, crate::inspector::InspectCategory::Properties)
            .unwrap();
        assert!(!core_xml(&tab).contains("dc:creator"), "{}", core_xml(&tab));
        assert_eq!(editor(&mut tab).core_fields().author, "");
        tab.set_dirty();
        assert!(!core_xml(&tab).contains("dc:creator"));
        assert!(editor(&mut tab).undo());
        tab.set_dirty();
        let xml = core_xml(&tab);
        assert!(!xml.contains("Bo") && !xml.contains("Ann"), "{xml}");
    }

    /// A new document has no package: the first Info edit gives it the one a
    /// save makes (as Mailings does), and its first save writes the values
    /// (escaped); a reopen shows them.
    #[test]
    fn a_new_document_is_given_a_package_and_saves_its_properties_1364() {
        let mut tab = crate::blank_docx_tab("Document1");
        assert!(tab.pkg.is_none());
        assert!(tab.ensure_core_part());
        assert!(tab.pkg.is_some());
        let ed = editor(&mut tab);
        assert!(ed.set_core_field(BoundProp::Title, "New & Shiny"));
        assert!(ed.set_core_field(BoundProp::Author, "Ann"));
        tab.mark_dirty();
        let path = temp("newdoc").join("new.docx");
        assert!(save_doc_tab(&mut tab, Some(path.clone())), "{}", tab.status);
        let core = part(&path, CORE_PART);
        assert!(core.contains("<dc:title>New &amp; Shiny</dc:title>"), "{core}");
        assert!(core.contains("<dc:creator>Ann</dc:creator>"), "{core}");
        let mut again = tab_from_path(&path);
        let f = editor(&mut again).core_fields().clone();
        assert_eq!((f.title.as_str(), f.author.as_str()), ("New & Shiny", "Ann"));
        // A new document nobody edited the properties of saves no core part.
        let mut plain = crate::blank_docx_tab("Document2");
        let path = temp("newdoc-plain").join("plain.docx");
        assert!(save_doc_tab(&mut plain, Some(path.clone())), "{}", plain.status);
        assert_eq!(part(&path, CORE_PART), "");
    }

    /// A tab that saves without the Word package (Markdown, Plain Text, Rich
    /// Text, an editable-HTML page) refuses, and stays package-less.
    #[test]
    fn a_tab_that_does_not_save_as_word_refuses_properties_1364() {
        let mut md = crate::blank_docx_tab("notes.md");
        md.markdown = true;
        assert!(!md.ensure_core_part());
        assert!(md.pkg.is_none());
        for name in ["a.txt", "a.rtf", "a.md", "a.docx.html"] {
            let mut tab = crate::blank_docx_tab(name);
            tab.path = Some(PathBuf::from(name));
            assert!(!tab.ensure_core_part(), "{name}");
            assert!(tab.pkg.is_none(), "{name}");
        }
        let mut word = crate::blank_docx_tab("a.docx");
        word.path = Some(PathBuf::from("a.docx"));
        assert!(word.ensure_core_part());
    }

    /// The package Mailings gives a new document before any Info edit holds
    /// no properties, so nothing is wiped; an Info edit after it keeps its
    /// undo history, and one before it leaves Mailings the package it made.
    #[test]
    fn mailings_and_info_share_the_new_documents_package_1364() {
        let mut tab = crate::blank_docx_tab("Document1");
        let ed = editor(&mut tab);
        ed.insert_str("Body");
        let bytes = crate::doc_to_docx(&editor(&mut tab).doc.clone(), &[], None);
        tab.pkg = Some(docxcore::package::load_package(&bytes).unwrap());
        assert!(tab.ensure_core_part());
        assert!(editor(&mut tab).set_core_field(BoundProp::Title, "T"));
        tab.mark_dirty();
        assert!(core_xml(&tab).contains("<dc:title>T</dc:title>"));
        // Mailings' lazy package is a no-op once there is one.
        let made = core_xml(&tab);
        assert!(tab.ensure_core_part());
        assert_eq!(core_xml(&tab), made);
        assert!(editor(&mut tab).undo());
        tab.set_dirty();
        assert!(!core_xml(&tab).contains("<dc:title>T</dc:title>"));
        assert_eq!(editor(&mut tab).core_fields().title, "");
        assert!(editor(&mut tab).undo(), "the typing step survived the seed");
        assert_eq!(editor(&mut tab).doc.body[0].plain_text(), "");
    }

    /// A Title control inserted after an Info edit shows it, and typing in
    /// the control updates Info and core.xml.
    #[test]
    fn a_title_control_and_info_agree_on_a_new_document_1364() {
        let mut tab = crate::blank_docx_tab("Document1");
        assert!(tab.ensure_core_part());
        assert!(editor(&mut tab).set_core_field(BoundProp::Title, "Info"));
        tab.mark_dirty();
        let control = crate::hf_insert::property_control(&tab, BoundProp::Title);
        let shown: String = control
            .iter()
            .map(|i| match i {
                docxcore::model::Inline::Run(r) => r.text.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(shown.contains("Info"), "{shown:?}");
    }

    /// A property control typed in updates what Info shows and core.xml,
    /// through `set_dirty`, without a step of Info's.
    #[test]
    fn typing_in_a_bound_control_updates_the_editors_field_and_core_xml() {
        use docxcore::model::{Block, Document, Paragraph, ParProps, RunProps};
        let (_, mut tab) = open("control", Some(CORE));
        let pkg = tab.pkg.as_ref().unwrap();
        let fields = core_fields_of(pkg);
        let body = vec![Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: docxcore::sdt::core_property_control(
                BoundProp::Title,
                "Old",
                1,
                &RunProps::default(),
            ),
        })];
        let mut ed = docxcore::editor::Editor::new(Document { body });
        ed.reseed_core_fields(fields);
        tab.surface = Surface::Doc(ed);
        let ed = editor(&mut tab);
        ed.set_caret(docxcore::editor::Caret::at(vec![0], 3));
        ed.insert_char('!');
        let steps = ed.undo_serial();
        tab.set_dirty();
        assert!(core_xml(&tab).contains("<dc:title>Old!</dc:title>"), "{}", core_xml(&tab));
        let ed = editor(&mut tab);
        assert_eq!(ed.core_fields().title, "Old!");
        assert_eq!(ed.core_unsynced(), []);
        assert_eq!(ed.undo_serial(), steps, "no step of Info's");
        // Undoing the typing puts the file back with the control.
        assert!(ed.undo());
        tab.set_dirty();
        assert!(core_xml(&tab).contains("<dc:title>Old</dc:title>"), "{}", core_xml(&tab));
        assert_eq!(editor(&mut tab).core_fields().title, "Old");
    }

    /// `Docxy` cannot be built in a unit test, so the sites that must apply
    /// a typed property are read: each of these routes calls
    /// `info_edit_apply` (or the Tab hook), or a typed value is skipped by
    /// Save, lost by Close, or written back by Remove All (#1364).
    #[test]
    fn the_sites_that_must_apply_a_typed_property_do() {
        let main = include_str!("main.rs");
        let close = include_str!("close.rs");
        let body = |src: &'static str, sig: &str| -> &'static str {
            let at = src.find(sig).unwrap_or_else(|| panic!("no {sig}"));
            let rest = &src[at + sig.len()..];
            let end = rest.find("\n    fn ").or_else(|| rest.find("\n    pub(crate) fn ")).unwrap();
            &rest[..end]
        };
        for (src, sig) in [
            (main, "fn save_active("),
            (main, "fn save_as("),
            (main, "fn backstage_rail_action("),
            (close, "fn commit_dialog_buffers_for_exit("),
        ] {
            assert!(body(src, sig).contains("info_edit_apply"), "{sig}");
        }
        assert!(body(main, "fn tab_key(").contains("info_edit_tab(true"));
        assert!(body(main, "fn shift_tab_key(").contains("info_edit_tab(false"));
        let remove = &main[main.find("Remove All\")").expect("Remove All button")..];
        let remove = &remove[..remove.find("cx.notify()").unwrap()];
        let apply = remove.find("info_edit_apply").expect("apply before removal");
        assert!(apply < remove.find("inspect_remove_active").unwrap());
        // Opening a field clears the note before it applies the field it
        // leaves, whose refusal must survive.
        let info = include_str!("info_props.rs");
        let open = body(info, "fn info_edit_open(");
        assert!(open.find("info_note = None").unwrap() < open.find("info_edit_apply").unwrap());
    }
}
