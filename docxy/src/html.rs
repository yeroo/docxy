//! Editable HTML (`sample.docx.html`) for the terminal editor.
//!
//! Opening and saving an existing bundle go through [`htmlbundle::unwrap`] and
//! [`htmlbundle::rewrap`], which need no engine. Making a *new* bundle embeds
//! the `docxwasm` engine, which is only compiled in with the `html-export`
//! feature (see `build.rs`); without it, export says so instead.

use std::path::Path;

#[cfg(feature = "html-export")]
const ENGINE: Option<&[u8]> = Some(include_bytes!(concat!(env!("OUT_DIR"), "/docxwasm.wasm")));
#[cfg(not(feature = "html-export"))]
const ENGINE: Option<&[u8]> = None;

/// What export says when the engine was not compiled in.
pub const NO_ENGINE: &str =
    "this docxy was built without HTML export (rebuild with --features html-export)";

/// Whether this build can make new bundles.
pub fn can_export() -> bool {
    ENGINE.is_some()
}

/// Wrap `docx` (the package bytes of `source_name`) as a new editable-HTML
/// bundle.
pub fn export(source_name: &str, docx: &[u8]) -> Result<String, String> {
    let engine = ENGINE.ok_or(NO_ENGINE)?;
    htmlbundle::wrap(
        &htmlbundle::docx_assets(),
        engine,
        "docx",
        source_name,
        docx,
        env!("CARGO_PKG_VERSION"),
        &htmlbundle::now_utc(),
    )
    .map_err(|e| e.to_string())
}

/// An opened bundle: its HTML (kept for [`htmlbundle::rewrap`] on save), the
/// embedded package, and the "changed since export" warning, if any.
pub struct Opened {
    pub html: String,
    pub docx: Vec<u8>,
    pub warning: Option<String>,
}

/// Read a `.docx.html` bundle from disk.
pub fn open(path: &str) -> Result<Opened, String> {
    let html = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let bundle = htmlbundle::unwrap(&html).map_err(|e| format!("{path}: {e}"))?;
    if bundle.meta.format() != "docx" {
        return Err(format!(
            "{path} holds a {} document, not a Word document",
            bundle.meta.format()
        ));
    }
    let warning = htmlbundle::sibling_warning(Path::new(path), &bundle.meta);
    Ok(Opened {
        html,
        docx: bundle.payload,
        warning,
    })
}

/// The file name (no directory) of `path`.
pub fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}
