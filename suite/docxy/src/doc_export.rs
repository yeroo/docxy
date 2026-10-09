//! File > Export > Change File Type (#635) and the document Save As
//! dialog's types (#635, #636).
//!
//! Choosing a type on the Export page opens Save As with that type's filter
//! first and the name carrying its extension (a template starts in the
//! personal templates folder); the save itself is Save As's, so the tab is
//! rebound to the new file and the next Save writes that type again. The
//! format always follows the name picked ([`crate::html_bundle::doc_target`]):
//! the dialog cannot report a filter change, so choosing another type in the
//! dialog without changing the name's extension keeps the name's type.

use super::*;

/// A type a document can be saved as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveFormat {
    Docx,
    Markdown,
    Html,
    Rtf,
    Text,
    Template,
    MacroTemplate,
}

/// The Save As dialog's types, in its order.
const DIALOG_ORDER: [SaveFormat; 7] = [
    SaveFormat::Docx,
    SaveFormat::Markdown,
    SaveFormat::Html,
    SaveFormat::Rtf,
    SaveFormat::Text,
    SaveFormat::Template,
    SaveFormat::MacroTemplate,
];

/// The Export page's Change File Type list, in its order.
const EXPORT_ORDER: [SaveFormat; 6] = [
    SaveFormat::Docx,
    SaveFormat::Template,
    SaveFormat::Text,
    SaveFormat::Rtf,
    SaveFormat::Markdown,
    SaveFormat::Html,
];

impl SaveFormat {
    /// The harness's name for it, and the Export page button's id suffix.
    pub(crate) fn key(self) -> &'static str {
        match self {
            SaveFormat::Docx => "docx",
            SaveFormat::Markdown => "md",
            SaveFormat::Html => "html",
            SaveFormat::Rtf => "rtf",
            SaveFormat::Text => "txt",
            SaveFormat::Template => "dotx",
            SaveFormat::MacroTemplate => "dotm",
        }
    }

    /// The Save As dialog filter's name.
    fn filter_name(self) -> &'static str {
        match self {
            SaveFormat::Docx => "Word document",
            SaveFormat::Markdown => "Markdown",
            SaveFormat::Html => "Editable HTML (*.docx.html)",
            SaveFormat::Rtf => "Rich Text Format",
            SaveFormat::Text => "Plain Text",
            SaveFormat::Template => "Word Template",
            SaveFormat::MacroTemplate => "Word Macro-Enabled Template",
        }
    }

    fn extensions(self) -> &'static [&'static str] {
        match self {
            SaveFormat::Docx => &["docx"],
            SaveFormat::Markdown => &["md", "markdown"],
            SaveFormat::Html => &["html"],
            SaveFormat::Rtf => &["rtf"],
            SaveFormat::Text => &["txt"],
            SaveFormat::Template => &["dotx"],
            SaveFormat::MacroTemplate => &["dotm"],
        }
    }

    /// The suffix a name of this type ends in.
    fn suffix(self) -> &'static str {
        match self {
            SaveFormat::Docx => ".docx",
            SaveFormat::Markdown => ".md",
            SaveFormat::Html => ".docx.html",
            SaveFormat::Rtf => ".rtf",
            SaveFormat::Text => ".txt",
            SaveFormat::Template => ".dotx",
            SaveFormat::MacroTemplate => ".dotm",
        }
    }

    /// The Export page's name and description.
    fn export_label(self) -> (&'static str, &'static str) {
        match self {
            SaveFormat::Docx => ("Word Document (*.docx)", "Word's document format"),
            SaveFormat::Template => (
                "Word Template (*.dotx)",
                "A starting point for new documents",
            ),
            SaveFormat::Text => ("Plain Text (*.txt)", "The text only, without formatting"),
            SaveFormat::Rtf => (
                "Rich Text Format (*.rtf)",
                "Keeps the text formatting most word processors read",
            ),
            SaveFormat::Markdown => ("Markdown (*.md)", "Plain text with light formatting"),
            SaveFormat::Html => (
                "Editable HTML (*.docx.html)",
                "A web page that opens and edits the document",
            ),
            SaveFormat::MacroTemplate => (
                "Word Macro-Enabled Template (*.dotm)",
                "A template that can hold macros",
            ),
        }
    }

    fn is_template(self) -> bool {
        matches!(self, SaveFormat::Template | SaveFormat::MacroTemplate)
    }
}

/// The Save As dialog's types: `preset` first, then the others in the
/// dialog's order; Editable HTML only when `html` (the build or the tab can
/// write one).
pub(crate) fn save_filters(html: bool, preset: Option<SaveFormat>) -> Vec<SaveFormat> {
    let shown = |f: &SaveFormat| html || *f != SaveFormat::Html;
    preset
        .into_iter()
        .chain(DIALOG_ORDER.into_iter().filter(|f| Some(*f) != preset))
        .filter(shown)
        .collect()
}

/// The Export page's types; Editable HTML only when `html`.
pub(crate) fn export_types(html: bool) -> Vec<SaveFormat> {
    EXPORT_ORDER
        .into_iter()
        .filter(|f| html || *f != SaveFormat::Html)
        .collect()
}

/// `name` with its document extension (`.docx`, `.md`, `.docx.html`, …)
/// replaced by `format`'s.
pub(crate) fn preset_name(name: &str, format: SaveFormat) -> String {
    let lower = name.to_ascii_lowercase();
    let known = [
        ".docx.html",
        ".html",
        ".htm",
        ".docx",
        ".docm",
        ".dotx",
        ".dotm",
        ".markdown",
        ".mdown",
        ".md",
        ".rtf",
        ".txt",
    ];
    let stem = known
        .iter()
        .find(|ext| lower.ends_with(*ext) && lower.len() > ext.len())
        .map_or(name, |ext| &name[..name.len() - ext.len()]);
    format!("{stem}{}", format.suffix())
}

impl Docxy {
    /// File > Export for a document: the Change File Type page. The rail item
    /// and the harness's `backstage-page {"page":"export"}` both come here.
    pub(crate) fn open_export(&mut self, cx: &mut Context<Self>) {
        self.bs_export = true;
        self.bs_new = false;
        self.bs_info = false;
        self.bs_info_status = None;
        self.bs_account = false;
        self.reset_backstage_scroll();
        cx.notify();
    }

    /// A Change File Type choice: Save As with that type preset. Never in a
    /// harness instance, which has no native dialogs (see [`Docxy::save_as`]);
    /// its `save-as` verb saves as any of these types.
    pub(crate) fn export_as(
        &mut self,
        format: SaveFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.active_is_doc() {
            return;
        }
        if doc_save_as_target(self.harness) == DocSaveTarget::RefuseHarness {
            self.set_status(DOC_SAVE_AS_HARNESS);
            return self.refocus(window, cx);
        }
        match self.pick_doc_save_target(Some(format)) {
            Some(target) => self.save_doc(Some(target), window, cx),
            None => self.refocus(window, cx),
        }
    }

    /// Ask for a document Save As destination, `preset` (an Export page
    /// choice) as the first type, the name's extension and, for a template,
    /// the personal templates folder, made if it is not there yet. The caller
    /// saves to it (the tab is rebound only once that save succeeds) and owns
    /// the cancellation status and any harness guard against native dialogs.
    pub(crate) fn pick_doc_save_target(&self, preset: Option<SaveFormat>) -> Option<PathBuf> {
        let tab = self.tabs.get(self.active);
        let name = tab
            .map(doc_save_as_name)
            .unwrap_or_else(|| "Document1.docx".into());
        let name = match preset {
            Some(format) => preset_name(&name, format),
            None => name,
        };
        // An open bundle can always be saved as one (it rewraps itself); a new
        // one needs the engine this build may not carry.
        let html = tab.is_some_and(doc_html_save_allowed);
        let mut dialog = rfd::FileDialog::new();
        for format in save_filters(html, preset) {
            dialog = dialog.add_filter(format.filter_name(), format.extensions());
        }
        let templates = preset
            .filter(|f| f.is_template())
            .map(|_| doc_templates::templates_dir())
            .filter(|dir| std::fs::create_dir_all(dir).is_ok());
        // A saved document opens beside its file (#1144), so an imported
        // one's .docx goes beside its Word 97-2003 original (#634; the save
        // refuses the original itself, whatever its name).
        if let Some(dir) = templates.or_else(|| tab.and_then(doc_import::save_start_dir_of)) {
            dialog = dialog.set_directory(dir);
        }
        // The name is written as picked (the dialog already asked about
        // overwriting it): any .html name saves a bundle, found by its content
        // when opened again.
        crate::macos_menu::native_modal(|| dialog.set_file_name(name).save_file())
    }

    /// The Export page. `None` unless it is selected and the active tab is a
    /// document.
    pub(crate) fn export_page(
        &self,
        bg: Hsla,
        fg: Hsla,
        dim: Hsla,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.bs_export {
            return None;
        }
        let tab = self
            .tabs
            .get(self.active)
            .filter(|t| t.kind == Kind::Docx)?;
        let mut rows = v_flex().gap_3();
        for format in export_types(doc_html_save_allowed(tab)) {
            let (name, sub) = format.export_label();
            rows = rows.child(
                v_flex()
                    .id(SharedString::from(format!("bs-export-{}", format.key())))
                    .w(px(420.))
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(dim)
                    .cursor_pointer()
                    .hover(|d| d.border_color(rgb(BRAND)))
                    .child(
                        div()
                            .text_color(fg)
                            .font_weight(FontWeight::BOLD)
                            .child(name),
                    )
                    .child(div().text_size(px(12.)).text_color(dim).child(sub))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.export_as(format, window, cx)),
                    ),
            );
        }
        Some(
            v_flex()
                .w_full()
                .p_8()
                .gap_4()
                .bg(bg)
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(fg)
                        .child("Export"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(BRAND))
                        .child("Change File Type"),
                )
                .child(rows)
                .into_any_element(),
        )
    }

    /// File > New's Personal templates: the templates found when the page
    /// was opened, each opening a new document from it.
    pub(crate) fn personal_templates_section(
        &self,
        fg: Hsla,
        dim: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = v_flex().gap_1();
        if self.bs_templates.is_empty() {
            list = list.child(div().text_size(px(12.)).text_color(dim).child(
                "No personal templates. Save a document as a Word Template to see it here.",
            ));
        }
        for (i, path) in self.bs_templates.iter().enumerate() {
            let path = path.clone();
            list = list.child(
                div()
                    .id(("bs-template", i))
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .rounded_sm()
                    .text_color(fg)
                    .hover(|d| d.text_color(rgb(BRAND)))
                    .child(format!(
                        "{} {}",
                        Kind::Docx.glyph(),
                        doc_templates::template_label(&path)
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.new_from_template_clicked(&path, window, cx)
                    })),
            );
        }
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(BRAND))
                    .mt_4()
                    .child("Personal templates"),
            )
            .child(list)
            .into_any_element()
    }

    fn new_from_template_clicked(
        &mut self,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = self.new_from_template(path, window, cx) {
            self.set_status(e);
            cx.notify();
        }
    }

    /// A new, untitled document from the template at `path` (File > New's
    /// Personal templates, and the harness's `new-from-template`), shown with
    /// the File screen closed. A file that is no template, or one that
    /// cannot be read, opens nothing and says why.
    pub(crate) fn new_from_template(
        &mut self,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let trusted = trusted::TrustStore::load(&config_root());
        let tab = doc_templates::template_tab(path, &trusted)?;
        self.cancel_highlight_mode();
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.drop_grid_state();
        self.backstage = false;
        self.show_backstage_open_page();
        self.persist(cx);
        self.refocus(window, cx);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn plain_save_as_keeps_its_order_and_adds_the_new_types_after() {
        let keys = |v: Vec<SaveFormat>| v.into_iter().map(SaveFormat::key).collect::<Vec<_>>();
        assert_eq!(
            keys(save_filters(true, None)),
            ["docx", "md", "html", "rtf", "txt", "dotx", "dotm"]
        );
        assert_eq!(
            keys(save_filters(false, None)),
            ["docx", "md", "rtf", "txt", "dotx", "dotm"]
        );
    }

    #[test]
    fn a_preset_type_comes_first_and_only_once() {
        let keys = |v: Vec<SaveFormat>| v.into_iter().map(SaveFormat::key).collect::<Vec<_>>();
        assert_eq!(
            keys(save_filters(true, Some(SaveFormat::Rtf))),
            ["rtf", "docx", "md", "html", "txt", "dotx", "dotm"]
        );
        assert_eq!(
            keys(save_filters(false, Some(SaveFormat::Template)))[0],
            "dotx"
        );
    }

    #[test]
    fn the_export_page_lists_words_change_file_type() {
        let keys = |v: Vec<SaveFormat>| v.into_iter().map(SaveFormat::key).collect::<Vec<_>>();
        assert_eq!(
            keys(export_types(true)),
            ["docx", "dotx", "txt", "rtf", "md", "html"]
        );
        assert_eq!(
            keys(export_types(false)),
            ["docx", "dotx", "txt", "rtf", "md"]
        );
    }

    #[test]
    fn a_preset_name_carries_its_extension() {
        assert_eq!(preset_name("Report.docx", SaveFormat::Rtf), "Report.rtf");
        assert_eq!(preset_name("Report.docx", SaveFormat::Text), "Report.txt");
        assert_eq!(
            preset_name("Report.DOCX", SaveFormat::Template),
            "Report.dotx"
        );
        assert_eq!(
            preset_name("Report.docx.html", SaveFormat::Docx),
            "Report.docx"
        );
        assert_eq!(preset_name("notes.md", SaveFormat::Html), "notes.docx.html");
        assert_eq!(preset_name("a.b.rtf", SaveFormat::Markdown), "a.b.md");
        assert_eq!(
            preset_name("Letter", SaveFormat::MacroTemplate),
            "Letter.dotm"
        );
        assert_eq!(preset_name(".docx", SaveFormat::Text), ".docx.txt");
    }

    /// Every Save As type writes what it says: the name it gives names that
    /// format ([`html_bundle::doc_target`]), and the save allows it.
    #[test]
    fn each_type_saves_as_what_its_name_says() {
        use html_bundle::DocTarget;
        for format in DIALOG_ORDER {
            let name = preset_name("x.docx", format);
            let path = std::path::Path::new(&name);
            assert!(doc_target_allowed(path), "{name}");
            let want = match format {
                SaveFormat::Docx => DocTarget::Docx(Some(docxcore::package::DocKind::Document)),
                SaveFormat::Markdown => DocTarget::Markdown,
                SaveFormat::Html => DocTarget::Html,
                SaveFormat::Rtf => DocTarget::Rtf,
                SaveFormat::Text => DocTarget::Text,
                SaveFormat::Template => DocTarget::Docx(Some(docxcore::package::DocKind::Template)),
                SaveFormat::MacroTemplate => {
                    DocTarget::Docx(Some(docxcore::package::DocKind::MacroTemplate))
                }
            };
            assert_eq!(
                html_bundle::doc_target(path, is_markdown_path(path)),
                want,
                "{name}"
            );
            assert!(name.ends_with(format.extensions()[0]), "{name}");
        }
    }
}
