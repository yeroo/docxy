//! Editable HTML (`sample.docx.html`) in the desktop suite: what Open, Save
//! and Save As do with a bundle. The pieces here are pure (paths and bytes in,
//! bytes or errors out) so they are unit-tested without a window or a dialog.
//!
//! Opening and re-saving a bundle need no engine ([`htmlbundle::unwrap`],
//! [`htmlbundle::rewrap`]); making a new one embeds the `docxwasm` engine, which
//! only the `html-export` feature compiles in (see `build.rs`).

use std::path::Path;

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

/// The format a document path saves as: any `.html`/`.htm` is a bundle (never
/// OOXML or Markdown written over an HTML name), Markdown by flag, Word
/// otherwise.
pub(crate) fn doc_target(path: &Path, markdown: bool) -> DocTarget {
    if htmlbundle::is_html_path(&path.to_string_lossy()) {
        DocTarget::Html
    } else if markdown {
        DocTarget::Markdown
    } else {
        DocTarget::Docx
    }
}

/// A bundle opened from disk (by its content, whatever it is called): its
/// HTML, the embedded Word package, and the "changed since export" note when
/// the original beside it no longer matches.
pub(crate) struct Opened {
    pub html: String,
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
        html,
    })
}

/// The page to write at `path` around `docx`:
///
/// 1. the bundle this document was opened from (`opened`), rewrapped, so its
///    engine and UI carry over, including on Save As to another name;
/// 2. else the bundle already at `path` (a tab restored after a restart),
///    rewrapped;
/// 3. else a new bundle, which needs the engine.
///
/// A file at `path` that is not a bundle is never overwritten.
pub(crate) fn bundle_bytes(
    path: &Path,
    opened: Option<&str>,
    docx: &[u8],
) -> Result<String, String> {
    if let Some(old) = opened {
        return htmlbundle::rewrap(old, docx).map_err(|e| e.to_string());
    }
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Ok(existing) = std::fs::read_to_string(path) {
        return match htmlbundle::rewrap(&existing, docx) {
            Ok(page) => Ok(page),
            Err(htmlbundle::Error::NotABundle) => Err(format!(
                "not overwriting {file}: it is not a docxy editable HTML file"
            )),
            Err(e) => Err(format!("{file}: {e}")),
        };
    }
    let engine = ENGINE.ok_or(
        "this build of docxy cannot make editable HTML (build it with --features html-export)",
    )?;
    htmlbundle::wrap(
        &htmlbundle::docx_assets(),
        engine,
        "docx",
        &htmlbundle::docx_source_name(&file),
        docx,
        env!("CARGO_PKG_VERSION"),
        &htmlbundle::now_utc(),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
    fn every_html_name_saves_as_a_bundle() {
        for html in [
            "a/sample.docx.html",
            "a/SAMPLE.DOCX.HTML",
            "a/sample.docx (1).html",
            "a/sample.docx(1).html",
            "a/notes.html",
            "a/page.htm",
        ] {
            assert_eq!(
                doc_target(Path::new(html), false),
                DocTarget::Html,
                "{html}"
            );
            assert_eq!(doc_target(Path::new(html), true), DocTarget::Html, "{html}");
        }
        assert_eq!(
            doc_target(Path::new("a/notes.md"), true),
            DocTarget::Markdown
        );
        assert_eq!(
            doc_target(Path::new("a/sample.docx"), false),
            DocTarget::Docx
        );
    }

    #[test]
    fn saving_a_bundle_rewraps_it() {
        let dir = temp("rewrap");
        let path = dir.join("sample.docx.html");
        let original = bundle(b"PK one");
        std::fs::write(&path, &original).unwrap();
        // A tab restored after a restart holds no bundle: the file is rewrapped.
        let html = bundle_bytes(&path, None, b"PK two").unwrap();
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
        let opened = std::fs::read_to_string(&from).unwrap();
        // The destination exists and is a different bundle: the opened one wins.
        std::fs::write(&to, bundle(b"PK other")).unwrap();
        let html = bundle_bytes(&to, Some(&opened), b"PK two").unwrap();
        assert!(html.contains(&htmlbundle::base64::encode(b"\0asm stand-in")));
        assert_eq!(htmlbundle::unwrap(&html).unwrap().payload, b"PK two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_bundle_needs_the_engine() {
        let dir = temp("new");
        let to = dir.join("fresh.docx.html");
        match bundle_bytes(&to, None, b"PK new") {
            Ok(page) => {
                assert!(can_export());
                let b = htmlbundle::unwrap(&page).unwrap();
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
    fn a_plain_html_file_is_never_overwritten() {
        let dir = temp("plain");
        let page = dir.join("page.html");
        std::fs::write(&page, "<html>mine</html>").unwrap();
        let err = bundle_bytes(&page, None, b"PK").unwrap_err();
        assert!(err.contains("not overwriting page.html"), "{err}");
        assert!(open(&page).is_err());
        assert_eq!(std::fs::read_to_string(&page).unwrap(), "<html>mine</html>");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn renamed_bundles_open_by_content() {
        let dir = temp("renamed");
        for name in ["sample.docx (1).html", "sample.docx(1).html", "notes.html"] {
            let path = dir.join(name);
            std::fs::write(&path, bundle(b"PK one")).unwrap();
            let o = open(&path).unwrap();
            assert_eq!(o.docx, b"PK one", "{name}");
            let page = bundle_bytes(&path, Some(&o.html), b"PK two").unwrap();
            assert_eq!(htmlbundle::unwrap(&page).unwrap().payload, b"PK two");
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
