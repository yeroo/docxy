//! The editable-HTML command line: `docxy x.docx --html out.docx.html` and
//! getting the Word file back out with `--docx`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn docxy(args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_docxy"))
        .args(args)
        .output()
        .expect("run docxy")
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("docxy-html-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_docx() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../assets/sample.docx"
    ))
    .unwrap()
}

/// A bundle made without an engine (reading one never needs it).
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
fn an_unedited_bundle_gives_back_the_exact_docx() {
    let dir = temp("lossless");
    let html = dir.join("sample.docx.html");
    std::fs::write(&html, bundle(&sample_docx())).unwrap();
    let back = dir.join("back.docx");
    let out = docxy(&[&html, Path::new("--docx"), &back]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&back).unwrap(),
        sample_docx(),
        "byte-identical"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn other_conversions_read_the_embedded_docx() {
    let dir = temp("md");
    let html = dir.join("sample.docx.html");
    std::fs::write(&html, bundle(&sample_docx())).unwrap();
    let md = dir.join("sample.md");
    let out = docxy(&[&html, Path::new("--md"), &md]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(std::fs::read_to_string(&md).unwrap().starts_with("# docxy"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tampered_bundle_is_refused() {
    let dir = temp("tampered");
    let html = dir.join("sample.docx.html");
    let good = bundle(b"PK original");
    let bad = good.replace(
        &htmlbundle::base64::encode(b"PK original"),
        &htmlbundle::base64::encode(b"PK tampered"),
    );
    std::fs::write(&html, bad).unwrap();
    let out = docxy(&[&html, Path::new("--docx"), &dir.join("x.docx")]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("integrity check"), "{err}");
    assert!(!dir.join("x.docx").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_spreadsheet_bundle_is_pointed_at_xlsxy() {
    let dir = temp("xlsx");
    let html = dir.join("book.xlsx.html");
    std::fs::write(&html, "<html></html>").unwrap();
    let out = docxy(&[&html, Path::new("--docx"), &dir.join("x.docx")]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("xlsxy"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(feature = "html-export"))]
#[test]
fn html_export_says_it_was_not_built_in() {
    let dir = temp("nofeature");
    let docx = dir.join("sample.docx");
    std::fs::write(&docx, sample_docx()).unwrap();
    let out = docxy(&[&docx, Path::new("--html"), &dir.join("sample.docx.html")]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--features html-export"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "html-export")]
#[test]
fn html_export_writes_one_offline_file_around_the_original_bytes() {
    let dir = temp("export");
    let docx = dir.join("sample.docx");
    std::fs::write(&docx, sample_docx()).unwrap();
    let out_path = dir.join("sample.docx.html");
    let out = docxy(&[&docx, Path::new("--html"), &out_path]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = std::fs::read_to_string(&out_path).unwrap();

    // The exact CSP: nothing may load or connect anywhere.
    assert!(html.contains(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; \
         script-src 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'unsafe-inline'; \
         img-src data: blob:; font-src data:\">"
    ));
    assert!(!html.contains("connect-src"));
    // No reference to any other file: every src/href is inline or a fragment.
    let lower = html.to_ascii_lowercase();
    for attr in [" src=", " href=", "<link"] {
        for (i, _) in lower.match_indices(attr) {
            let v = lower[i + attr.len()..].trim_start_matches(['"', '\'']);
            assert!(
                v.starts_with("data:") || v.starts_with('#'),
                "external reference after {attr}: {}",
                &html[i..(i + 60).min(html.len())]
            );
        }
    }
    // The stylesheet loads nothing either (`url(` in the scripts is JS, such
    // as `new URL(`, not a CSS fetch).
    let css_open = "<style id=\"docxy-css\">";
    let s = lower.find(css_open).unwrap() + css_open.len();
    let css = &lower[s..s + lower[s..].find("</style>").unwrap()];
    assert!(!css.contains("@import"));
    for (i, _) in css.match_indices("url(") {
        let v = css[i + 4..].trim_start_matches(['"', '\'']);
        assert!(
            v.starts_with("data:"),
            "stylesheet fetch: {}",
            &css[i..(i + 60).min(css.len())]
        );
    }
    // The payload is the original package, byte for byte, and the engine a
    // real wasm module.
    let b = htmlbundle::unwrap(&html).unwrap();
    assert_eq!(b.payload, sample_docx());
    assert_eq!(b.meta.source_name(), "sample.docx");
    let engine_open = "id=\"docxy-engine\">";
    let s = html.find(engine_open).unwrap() + engine_open.len();
    let e = s + html[s..].find("</script>").unwrap();
    let wasm = htmlbundle::base64::decode(&html[s..e]).unwrap();
    assert_eq!(&wasm[..4], b"\0asm");

    // And straight back out, unchanged.
    let back = dir.join("back.docx");
    let out = docxy(&[&out_path, Path::new("--docx"), &back]);
    assert!(out.status.success());
    assert_eq!(std::fs::read(&back).unwrap(), sample_docx());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "html-export")]
#[test]
fn html_export_from_a_bundle_keeps_its_original_name() {
    let dir = temp("rebundle");
    let old = dir.join("sample.docx.html");
    std::fs::write(&old, bundle(&sample_docx())).unwrap();
    let new = dir.join("fresh.docx.html");
    let out = docxy(&[&old, Path::new("--html"), &new]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let b = htmlbundle::unwrap(&std::fs::read_to_string(&new).unwrap()).unwrap();
    assert_eq!(b.meta.source_name(), "sample.docx");
    assert_eq!(b.payload, sample_docx());
    let _ = std::fs::remove_dir_all(&dir);
}
