//! Editable HTML (`sample.docx.html`) in the desktop suite: what Open, Save
//! and Save As do with a bundle. The pieces here are pure (paths and bytes in,
//! bytes or errors out) so they are unit-tested without a window or a dialog.
//!
//! Opening and re-saving a bundle need no engine ([`htmlbundle::unwrap`],
//! [`htmlbundle::rewrap`]); making a new one embeds the `docxwasm` engine, which
//! only the `html-export` feature compiles in (see `build.rs`).

use std::path::{Path, PathBuf};

#[cfg(feature = "html-export")]
const ENGINE: Option<&[u8]> = Some(include_bytes!(concat!(env!("OUT_DIR"), "/docxwasm.wasm")));
#[cfg(not(feature = "html-export"))]
const ENGINE: Option<&[u8]> = None;

/// Whether this build can make new bundles (and so offers them in Save As).
pub(crate) fn can_export() -> bool {
    ENGINE.is_some()
}

/// How a document tab is written, from the path it saves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocTarget {
    Docx,
    Markdown,
    Html,
}

/// The format a document path saves as: `*.docx.html` is a bundle, Markdown
/// by extension, Word otherwise.
pub(crate) fn doc_target(path: &Path, markdown: bool) -> DocTarget {
    if htmlbundle::is_docx_bundle_path(&path.to_string_lossy()) {
        DocTarget::Html
    } else if markdown {
        DocTarget::Markdown
    } else {
        DocTarget::Docx
    }
}

/// A Save As name picked with the "Editable HTML" filter must stay
/// recognizable as a bundle: `report.html` becomes `report.docx.html`, so the
/// original format is in the name (the `<original name>.html` convention).
pub(crate) fn normalize_save_target(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    let lower = s.to_ascii_lowercase();
    if !lower.ends_with(".html") || htmlbundle::bundle_inner_ext(&lower).is_some() {
        return path;
    }
    PathBuf::from(format!("{}.docx.html", &s[..s.len() - ".html".len()]))
}

/// A bundle opened from disk: the embedded Word package, and the "changed
/// since export" note when the original beside it no longer matches.
pub(crate) struct Opened {
    pub docx: Vec<u8>,
    pub warning: Option<String>,
}

pub(crate) fn open(path: &Path) -> Result<Opened, String> {
    let html = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
    let bundle = htmlbundle::unwrap(&html).map_err(|e| e.to_string())?;
    if bundle.meta.format() != "docx" {
        return Err(format!(
            "this file holds a {} document, not a Word document",
            bundle.meta.format()
        ));
    }
    Ok(Opened {
        warning: htmlbundle::sibling_warning(path, &bundle.meta),
        docx: bundle.payload,
    })
}

/// The bytes to write for a bundle at `path` around `docx`: rewrap the bundle
/// already there (or the one this document came from, on Save As), keeping its
/// engine and UI; otherwise make a new one, which needs the engine.
pub(crate) fn bundle_bytes(
    path: &Path,
    came_from: Option<&Path>,
    docx: &[u8],
) -> Result<Vec<u8>, String> {
    let existing = [Some(path), came_from]
        .into_iter()
        .flatten()
        .filter(|p| htmlbundle::is_docx_bundle_path(&p.to_string_lossy()))
        .find_map(|p| std::fs::read_to_string(p).ok());
    if let Some(old) = existing {
        return htmlbundle::rewrap(&old, docx)
            .map(String::into_bytes)
            .map_err(|e| e.to_string());
    }
    let engine = ENGINE.ok_or(
        "this build of docxy cannot make editable HTML (build it with --features html-export)",
    )?;
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let source = htmlbundle::source_name(&file)
        .unwrap_or("document.docx")
        .to_string();
    htmlbundle::wrap(
        &htmlbundle::docx_assets(),
        engine,
        "docx",
        &source,
        docx,
        env!("CARGO_PKG_VERSION"),
        &htmlbundle::now_utc(),
    )
    .map(String::into_bytes)
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("suite-html-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bundle(docx: &[u8]) -> String {
        htmlbundle::wrap(
            &htmlbundle::docx_assets(),
            b"\0asm stand-in",
            "docx",
            "sample.docx",
            docx,
            "test",
            "2026-09-25T00:00:00Z",
        )
        .unwrap()
    }

    #[test]
    fn bundle_paths_save_as_html() {
        assert_eq!(
            doc_target(Path::new("a/sample.docx.html"), false),
            DocTarget::Html
        );
        assert_eq!(
            doc_target(Path::new("a/SAMPLE.DOCX.HTML"), true),
            DocTarget::Html
        );
        assert_eq!(
            doc_target(Path::new("a/notes.md"), true),
            DocTarget::Markdown
        );
        assert_eq!(
            doc_target(Path::new("a/sample.docx"), false),
            DocTarget::Docx
        );
        assert_eq!(doc_target(Path::new("a/page.html"), false), DocTarget::Docx);
    }

    #[test]
    fn save_as_names_keep_the_original_format_in_them() {
        assert_eq!(
            normalize_save_target(PathBuf::from("d/report.html")),
            PathBuf::from("d/report.docx.html")
        );
        for keep in ["d/report.docx.html", "d/report.docx", "d/notes.md"] {
            assert_eq!(
                normalize_save_target(PathBuf::from(keep)),
                PathBuf::from(keep)
            );
        }
    }

    #[test]
    fn saving_a_bundle_rewraps_it() {
        let dir = temp("rewrap");
        let path = dir.join("sample.docx.html");
        let original = bundle(b"PK one");
        std::fs::write(&path, &original).unwrap();
        let bytes = bundle_bytes(&path, None, b"PK two").unwrap();
        let html = String::from_utf8(bytes).unwrap();
        let cut = |h: &str| h[..h.rfind(htmlbundle::PAYLOAD_OPEN).unwrap()].to_string();
        assert_eq!(cut(&html), cut(&original));
        let b = htmlbundle::unwrap(&html).unwrap();
        assert_eq!(b.payload, b"PK two");
        assert_eq!(
            b.meta.source_sha256(),
            htmlbundle::sha256::hex_digest(b"PK one")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_as_from_a_bundle_keeps_its_engine() {
        let dir = temp("saveas");
        let from = dir.join("sample.docx.html");
        std::fs::write(&from, bundle(b"PK one")).unwrap();
        let to = dir.join("copy.docx.html");
        let html = String::from_utf8(bundle_bytes(&to, Some(&from), b"PK two").unwrap()).unwrap();
        assert!(html.contains(&htmlbundle::base64::encode(b"\0asm stand-in")));
        assert_eq!(htmlbundle::unwrap(&html).unwrap().payload, b"PK two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_bundle_needs_the_engine() {
        let dir = temp("new");
        let to = dir.join("fresh.docx.html");
        let from = dir.join("fresh.docx");
        match bundle_bytes(&to, Some(&from), b"PK new") {
            Ok(bytes) => {
                assert!(can_export());
                let b = htmlbundle::unwrap(&String::from_utf8(bytes).unwrap()).unwrap();
                assert_eq!(b.meta.source_name(), "fresh.docx");
                assert_eq!(b.payload, b"PK new");
            }
            Err(e) => {
                assert!(!can_export());
                assert!(e.contains("--features html-export"), "{e}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_reads_the_package_and_notes_a_changed_original() {
        let dir = temp("open");
        let path = dir.join("sample.docx.html");
        std::fs::write(&path, bundle(b"PK one")).unwrap();
        let o = open(&path).unwrap();
        assert_eq!(o.docx, b"PK one");
        assert!(o.warning.is_none());
        std::fs::write(dir.join("sample.docx"), b"changed").unwrap();
        let w = open(&path).unwrap().warning.unwrap();
        assert!(w.starts_with("sample.docx changed since export"), "{w}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_bundle_is_refused() {
        let dir = temp("damaged");
        let path = dir.join("sample.docx.html");
        let bad = bundle(b"PK one").replace(
            &htmlbundle::base64::encode(b"PK one"),
            &htmlbundle::base64::encode(b"PK two"),
        );
        std::fs::write(&path, bad).unwrap();
        let err = open(&path).err().unwrap();
        assert!(err.contains("integrity"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
