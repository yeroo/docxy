//! Word templates (#636): where personal templates live, which ones File >
//! New lists, and the untitled document a template opens as.
//!
//! A `.dotx`/`.dotm` opens the way an Excel template does
//! ([`crate::template_title`]): as a new, never-saved document named
//! `<stem>1.docx` (`.docm` from a `.dotm`), the first such name free beside
//! the template, so the first Save asks for a name and never writes the
//! template. Editing a template itself in place is not offered.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use docxcore::package::DocKind;

use crate::{CONFIG_DIR_ENV, DocTab, Surface, config_root_from, file_name};

/// The operating systems whose Office keeps personal templates in its own
/// place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    Windows,
    Mac,
    Other,
}

impl Platform {
    fn current() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Other
        }
    }
}

/// The personal templates folder: where File > New looks and where Save As
/// a template starts. See [`templates_dir_from`].
pub(crate) fn templates_dir() -> PathBuf {
    templates_dir_from(
        std::env::var_os(CONFIG_DIR_ENV),
        dirs::document_dir(),
        dirs::home_dir(),
        dirs::config_dir(),
        Platform::current(),
    )
}

/// [`templates_dir`]'s decision, apart from the environment so it can be
/// tested. A `DOCXY_CONFIG_DIR` override wins on every OS, so a test or
/// harness instance never reads or writes the real folder:
/// `<override>/docxy/templates`. Otherwise Word's own: `Documents\Custom
/// Office Templates` on Windows, Office's group container on macOS, and
/// `<config>/docxy/templates` elsewhere (or when the OS folder is unknown).
pub(crate) fn templates_dir_from(
    over: Option<OsString>,
    documents: Option<PathBuf>,
    home: Option<PathBuf>,
    os_config: Option<PathBuf>,
    os: Platform,
) -> PathBuf {
    let own = |over| {
        config_root_from(over, os_config.clone())
            .join("docxy")
            .join("templates")
    };
    if over.as_ref().is_some_and(|v| !v.is_empty()) {
        return own(over);
    }
    match (os, documents, home) {
        (Platform::Windows, Some(docs), _) => docs.join("Custom Office Templates"),
        (Platform::Mac, _, Some(home)) => home
            .join("Library")
            .join("Group Containers")
            .join("UBF8T346G9.Office")
            .join("User Content")
            .join("Templates"),
        _ => own(None),
    }
}

/// The Word templates (`.dotx`, `.dotm`) directly in `dir`, by name without
/// regard to case. A missing folder lists none, and is not created.
pub(crate) fn personal_templates(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && DocKind::from_path(p).is_some_and(DocKind::is_template))
        .collect();
    found.sort_by_key(|p| file_name(p).to_lowercase());
    found
}

/// What File > New shows for a template: its name without the extension.
pub(crate) fn template_label(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name(path))
}

/// For a Word template, the title of the new document it opens as:
/// `Letter.dotx` gives `Letter1.docx`, or `Letter2.docx` when that exists
/// beside it; a `.dotm` gives `.docm`. `None` for anything else.
pub(crate) fn doc_template_title(path: &Path) -> Option<String> {
    let kind = DocKind::from_path(path).filter(|k| k.is_template())?;
    let ext = kind.document_kind().extension();
    let stem = path.file_stem()?.to_string_lossy();
    (1u32..)
        .map(|n| format!("{stem}{n}.{ext}"))
        .find(|name| !path.with_file_name(name).exists())
}

/// Make `tab`, just loaded from `path`, the untitled document a template
/// opens as, when `path` is a template and loaded as a Word package. One
/// that failed to load, was converted from another format, or holds no
/// package at all (an empty file) stays bound to its file, whose own guards
/// then keep Save off it. Whether it became untitled.
pub(crate) fn untitle_template_tab(tab: &mut DocTab, path: &Path) -> bool {
    let Some(title) = doc_template_title(path) else {
        return false;
    };
    if !matches!(tab.surface, Surface::Doc(_))
        || tab.load_failed
        || tab.access.converted.is_some()
        || tab.pkg.is_none()
    {
        return false;
    }
    tab.path = None;
    tab.title = title.into();
    tab.dirty = false;
    tab.status = format!("new document from template {}", file_name(path)).into();
    true
}

/// The new, untitled document File > New makes from the template at `path`,
/// or why it makes none: `path` is no Word template, or it does not load
/// as one (unreadable, damaged, empty, or another format under the name).
pub(crate) fn template_tab(
    path: &Path,
    trusted: &crate::trusted::TrustStore,
) -> Result<DocTab, String> {
    if doc_template_title(path).is_none() {
        return Err(format!(
            "\"{}\" is not a Word template (.dotx or .dotm)",
            file_name(path)
        ));
    }
    let tab = crate::tab_from_path_mode(&path.to_path_buf(), crate::OpenMode::Normal, trusted)?;
    if tab.path.is_some() {
        let why = if tab.load_failed {
            tab.status.to_string()
        } else {
            "it holds no Word document".into()
        };
        return Err(format!(
            "cannot open the template \"{}\": {why}",
            file_name(path)
        ));
    }
    Ok(tab)
}
