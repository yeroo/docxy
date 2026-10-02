//! Word 97-2003 documents (`.doc`) opened as imports (#634).
//!
//! `load_package` reads only `.docx`; an OLE2 file comes back as
//! `LoadError::Ole2`, and the document load then imports it with
//! [`docxcore::legacy::doc::import_doc`]. The routing goes by content, so a
//! `.doc` renamed `.docx` imports too. Like Word, the tab keeps two facts
//! apart:
//!
//! - the file it is bound to is binary ([`DocImport::binary_source`]), so
//!   Save never writes it: it asks for a `.docx` beside it, and a save to a
//!   `.doc` is refused outright;
//! - the document is in Compatibility Mode ([`DocImport::compat`]): the tab
//!   says `[Compatibility Mode]`, a save keeps `compatibilityMode` 11, and
//!   File > Info > Convert raises it to 15.

use std::path::Path;

use super::DocTab;

/// How a document tab relates to a Word 97-2003 file it was imported from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct DocImport {
    /// The tab's file is a binary Word 97-2003 document. Save asks where
    /// to write the `.docx`; cleared once a save rebinds the tab to a file
    /// it wrote.
    pub(super) binary_source: bool,
    /// The document is in Compatibility Mode until Convert.
    pub(super) compat: bool,
}

impl DocImport {
    /// A document just imported from a `.doc`.
    pub(super) const IMPORTED: DocImport = DocImport {
        binary_source: true,
        compat: true,
    };
}

/// The status line of a freshly imported document.
pub(super) const IMPORTED_STATUS: &str = "imported Word 97-2003 Document; saving writes .docx";

/// What the tab strip adds to a document in Compatibility Mode.
pub(super) const COMPAT_SUFFIX: &str = " [Compatibility Mode]";

/// What a harness instance says when Save of an imported document needs
/// the Save As dialog.
pub(super) const IMPORTED_HARNESS: &str = "This document was imported from a Word 97-2003 file and saves as .docx, and a harness instance cannot open the Save As dialog; use the harness save-as verb";

/// The status of a save refused because its target is a Word 97-2003 file.
pub(super) const BINARY_TARGET_REFUSED: &str =
    "Word 97-2003 files (.doc, .dot) cannot be written: save as .docx";

/// Whether `path` names a Word 97-2003 document or template, which no save
/// writes.
pub(super) fn is_binary_doc_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("doc") || e.eq_ignore_ascii_case("dot"))
}

/// Whether a save of `tab` to `target` (`None`: its own file) must not
/// write: the target is a `.doc`/`.dot`, or, in place, the tab's file is
/// the binary original.
pub(super) fn refuses_target(tab: &DocTab, target: Option<&Path>) -> bool {
    match target.or(tab.path.as_deref()) {
        Some(path) => is_binary_doc_path(path) || (target.is_none() && tab.import.binary_source),
        None => false,
    }
}

/// The name the Save As dialog suggests: an imported document's own stem
/// as `.docx` (Word's Save As on a `.doc`), anything else its title.
pub(super) fn save_name(tab: &DocTab) -> String {
    let binary = tab.import.binary_source || tab.path.as_deref().is_some_and(is_binary_doc_path);
    if !binary {
        return tab.title.to_string();
    }
    let name = tab
        .path
        .as_deref()
        .unwrap_or_else(|| Path::new(tab.title.as_ref()));
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    format!("{stem}.docx")
}

/// File > Info > Convert: take the document out of Compatibility Mode
/// (`compatibilityMode` 15). The tab is dirty after it, and the next save
/// writes the setting; `Err` says why nothing changed.
pub(super) fn convert_tab(tab: &mut DocTab) -> Result<String, String> {
    if !matches!(tab.surface, super::Surface::Doc(_)) {
        return Err("the active tab is not a document".into());
    }
    if !tab.import.compat {
        return Err("this document is not in Compatibility Mode: nothing to convert".into());
    }
    let pkg = tab
        .pkg
        .as_mut()
        .ok_or("this document has no package to convert")?;
    pkg.set_compatibility_mode(15);
    tab.import.compat = false;
    tab.dirty = true;
    let status = "converted to the newest file format; saving writes .docx".to_string();
    tab.status = status.clone().into();
    Ok(status)
}
