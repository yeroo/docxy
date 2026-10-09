//! Word 97-2003 documents (`.doc`) opened as imports (#634).
//!
//! `load_package` reads only `.docx`; an OLE2 file comes back as
//! `LoadError::Ole2`, and the document load then imports it with
//! [`docxcore::legacy::doc::import_doc`]. The routing goes by content, so a
//! `.doc` renamed `.docx` imports too. Like Word, the tab keeps two facts
//! apart:
//!
//! - the file it is bound to is binary ([`DocImport::binary_source`]), so
//!   no save writes it: Save asks for a `.docx`, offering one beside it,
//!   and a save to that file or to any `.doc` is refused outright;
//! - the document is in Compatibility Mode ([`DocImport::compat`]): the tab
//!   says `[Compatibility Mode]`, a save keeps `compatibilityMode` 11, and
//!   File > Info > Convert raises it to 15.

use std::path::{Path, PathBuf};

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

/// What a harness instance says when Save of a document bound to a `.doc` or
/// `.dot` path (not an import: a `.docx` named so) needs the Save As dialog.
pub(super) const BINARY_PATH_HARNESS: &str = "Word 97-2003 files (.doc, .dot) cannot be written, and a harness instance cannot open the Save As dialog; use the harness save-as verb to save as .docx";

/// Why a harness instance refuses a Save that needs the Save As dialog,
/// by its cause: the tab was imported from a Word 97-2003 file, it was
/// never saved, or its path is a `.doc`/`.dot` that nothing writes.
pub(super) fn harness_save_refusal(tab: &DocTab) -> &'static str {
    if tab.import.binary_source {
        IMPORTED_HARNESS
    } else if tab.path.is_none() {
        super::DOC_NEVER_SAVED_HARNESS
    } else {
        BINARY_PATH_HARNESS
    }
}

/// The status of a save refused because its target is a Word 97-2003 file.
pub(super) const BINARY_TARGET_REFUSED: &str =
    "Word 97-2003 files (.doc, .dot) cannot be written: save as .docx";

/// Whether `path` names a Word 97-2003 document or template, which no save
/// writes.
pub(super) fn is_binary_doc_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("doc") || e.eq_ignore_ascii_case("dot"))
}

/// The status of a Save As refused because its target is the binary file the
/// document was imported from (a `.doc` renamed `.docx`, say).
pub(super) const SOURCE_TARGET_REFUSED: &str = "this is the Word 97-2003 file the document was imported from: save the .docx under another name";

/// The status of a Save refused because the tab's own file is the binary
/// original.
pub(super) const IN_PLACE_REFUSED: &str =
    "this document was imported from a Word 97-2003 file: use Save As to save it as .docx";

/// Why a save of `tab` to `target` (`None`: its own file) must not write,
/// or `None` when it may: the target is a `.doc`/`.dot`, or it is the
/// binary original the tab was imported from, in place or picked again
/// (compared as the file system resolves it, so in any case on Windows).
pub(super) fn save_refusal(tab: &DocTab, target: Option<&Path>) -> Option<&'static str> {
    let path = target.or(tab.path.as_deref())?;
    if target.is_none() && tab.import.binary_source {
        return Some(IN_PLACE_REFUSED);
    }
    if is_binary_doc_path(path) {
        return Some(BINARY_TARGET_REFUSED);
    }
    match (target, tab.path.as_deref()) {
        (Some(target), Some(source)) if tab.import.binary_source && same_file(target, source) => {
            Some(SOURCE_TARGET_REFUSED)
        }
        _ => None,
    }
}

/// Whether two paths name one file: canonically, and on Windows (whose file
/// names ignore case) also when only the case differs.
fn same_file(a: &Path, b: &Path) -> bool {
    let (a, b) = (super::canonical(a), super::canonical(b));
    a == b
        || (cfg!(windows)
            && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase())
}

/// The name the Save As dialog suggests: an imported document's own stem
/// as `.docx` (Word's Save As on a `.doc`), anything else its title. When
/// that would be the binary original's own name (a `.doc` renamed `.docx`),
/// `<stem> (converted).docx`, so the suggestion never points at it.
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
    let docx = format!("{stem}.docx");
    let own = name
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&docx));
    if own {
        format!("{stem} (converted).docx")
    } else {
        docx
    }
}

/// The folder of an imported document's original, `None` for anything else.
/// Close's own-folder pick uses it; the Save As dialogs start in
/// [`save_start_dir_of`].
pub(super) fn save_dir(tab: &DocTab) -> Option<&Path> {
    tab.import
        .binary_source
        .then_some(tab.path.as_deref()?.parent()?)
        .filter(|dir| !dir.as_os_str().is_empty())
}

/// The folder a Save As dialog opens in for a file at `path`: its own folder
/// (#1144), made absolute. `None` for a new tab and for a bare filename,
/// which leave the dialog at the OS default.
pub(super) fn save_start_dir(path: Option<&Path>) -> Option<PathBuf> {
    let dir = path?.parent().filter(|dir| !dir.as_os_str().is_empty())?;
    // A relative folder means nothing to a dialog that may not share the cwd.
    Some(std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()))
}

/// [`save_start_dir`] for a tab.
pub(super) fn save_start_dir_of(tab: &DocTab) -> Option<PathBuf> {
    save_start_dir(tab.path.as_deref())
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
    tab.set_dirty();
    let status = "converted to the newest file format; saving writes .docx".to_string();
    tab.status = status.clone().into();
    Ok(status)
}
