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
    /// the package, and show it in the open header or footer's controls. A
    /// package without the part keeps them in the editor only.
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

    /// Whether the file has the part the properties are written to.
    fn has_core_part(&self) -> bool {
        self.pkg
            .as_ref()
            .is_some_and(|p| p.part_text(CORE_PART).is_some())
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
        self.info_edit_apply(cx);
        if self.active_locked() {
            self.info_note = Some(format!(
                "{} is read-only: this document is in Protected View or marked as final",
                which.label()
            ));
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
        self.info_note = None;
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
    /// elsewhere, leaving Info). An unchanged value records no step; a file
    /// without a core properties part refuses with a note.
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
        if !tab.has_core_part() {
            self.info_note = Some(format!(
                "Could not change {}: this file has no core properties part",
                e.which.label()
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

    /// Route a keystroke to the focused property field, as the Font box's.
    pub(crate) fn info_edit_key(
        &mut self,
        ev: &KeyDownEvent,
        ctrl: bool,
        alt: bool,
        key: &str,
        shift: bool,
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
            "tab" => {
                let next = e.which.step(!shift);
                self.info_edit_open(next, cx);
            }
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
            ("[Content_Types].xml", "<?xml version=\"1.0\"?><Types/>"),
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
        assert!(!bare.has_core_part());
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

    /// A control typed in updates what Info shows, without a step of Info's.
    #[test]
    fn typing_in_a_bound_control_updates_the_editors_field() {
        let (_, mut tab) = open("control", Some(CORE));
        let ed = editor(&mut tab);
        ed.note_core_field(BoundProp::Title, "Typed");
        assert_eq!(ed.core_fields().title, "Typed");
        assert_eq!(ed.core_unsynced(), []);
        assert!(ed.undo_serial().is_none());
    }
}
