use super::*;

const WHEN: &str = "2026-09-25T15:00:00Z";

fn test_assets() -> Assets<'static> {
    Assets {
        shell: include_str!("../web/shell.html"),
        css: "body { color: red; }\r\n.a > .b { x: 1 }\n",
        engine_js: "var ENGINE = 1;\n",
        app_js: "var APP = '{{css}}';\n",
        ribbon_json: "{\"icons\":{\"bold\":\"<svg><path d='M0'/></svg>\"}}\n",
    }
}

fn bundle(payload: &[u8]) -> String {
    wrap(
        &test_assets(),
        b"\0asm-engine",
        "docx",
        "sample.docx",
        payload,
        "0.5.0",
        WHEN,
    )
    .unwrap()
}

#[test]
fn wrap_then_unwrap_gives_back_the_exact_payload() {
    let payload: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let html = bundle(&payload);
    let b = unwrap(&html).unwrap();
    assert_eq!(b.payload, payload);
    assert_eq!(b.meta.format(), "docx");
    assert_eq!(b.meta.source_name(), "sample.docx");
    let digest = sha256::hex_digest(&payload);
    assert_eq!(b.meta.source_sha256(), digest);
    assert_eq!(b.meta.payload_sha256(), digest);
    assert_eq!(b.meta.get("docxyVersion"), Some("0.5.0"));
    assert_eq!(b.meta.get("exportedAt"), Some(WHEN));
}

#[test]
fn bundle_is_self_contained_and_offline() {
    let html = bundle(b"PK");
    assert!(html.contains(&format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">"
    )));
    assert!(CSP.starts_with("default-src 'none';"));
    assert!(!CSP.contains("connect-src"));
    // No attribute may point anywhere: every src/href is inline or a fragment.
    for attr in [" src=\"", " href=\"", " src='", " href='"] {
        for (i, _) in html.match_indices(attr) {
            let v = &html[i + attr.len()..];
            assert!(
                v.starts_with("data:") || v.starts_with('#'),
                "external reference: {}",
                &v[..v.len().min(40)]
            );
        }
    }
    assert!(!html.contains("http://") && !html.contains("https://"));
}

#[test]
fn file_is_lf_only() {
    let html = bundle(b"PK");
    assert!(!html.contains('\r'), "CRLF in an asset must be normalized");
}

#[test]
fn tampered_payload_is_rejected() {
    let html = bundle(b"original bytes");
    let tampered = html.replace(
        &base64::encode(b"original bytes"),
        &base64::encode(b"evil bytes!!!!"),
    );
    assert_ne!(tampered, html);
    match unwrap(&tampered) {
        Err(Error::IntegrityMismatch { expected, actual }) => {
            assert_eq!(expected, sha256::hex_digest(b"original bytes"));
            assert_eq!(actual, sha256::hex_digest(b"evil bytes!!!!"));
        }
        other => panic!("expected an integrity error, got {other:?}"),
    }
    let msg = unwrap(&tampered).unwrap_err().to_string();
    assert!(msg.contains("integrity"), "{msg}");
}

#[test]
fn plain_html_is_not_a_bundle() {
    assert_eq!(
        unwrap("<html><body>hi</body></html>"),
        Err(Error::NotABundle)
    );
}

#[test]
fn rewrap_changes_only_the_payload_and_its_hash() {
    let html = bundle(b"version one");
    let next = rewrap(&html, b"version two, longer").unwrap();
    let b = unwrap(&next).unwrap();
    assert_eq!(b.payload, b"version two, longer");
    assert_eq!(
        b.meta.payload_sha256(),
        sha256::hex_digest(b"version two, longer")
    );
    // The original's hash never changes: it is what "changed since export"
    // compares a sibling file against.
    assert_eq!(b.meta.source_sha256(), sha256::hex_digest(b"version one"));
    assert_eq!(b.meta.get("exportedAt"), Some(WHEN));

    // Everything before the payload block (engine, UI, template) is untouched.
    let cut = |h: &str| h[..h.rfind(PAYLOAD_OPEN).unwrap()].to_string();
    assert_eq!(cut(&html), cut(&next));
    assert!(next.ends_with("</script>\n</body>\n</html>\n"));

    // Rewrapping twice is stable.
    let again = rewrap(&next, b"version two, longer").unwrap();
    assert_eq!(again, next);
}

#[test]
fn rewrap_keeps_unknown_meta_keys_in_order() {
    let html = bundle(b"x");
    let (s, e) = payload_span(&html).unwrap();
    let mut meta = unwrap(&html).unwrap().meta;
    meta.set("futureKey", "kept");
    let edited = format!(
        "{}\n{}\n{}\n{}",
        &html[..s],
        meta.to_json(),
        base64::encode(b"x"),
        &html[e..]
    );
    let next = rewrap(&edited, b"y").unwrap();
    let keys: Vec<String> = unwrap(&next)
        .unwrap()
        .meta
        .fields
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(
        keys,
        [
            "format",
            "sourceName",
            "sourceSha256",
            "payloadSha256",
            "docxyVersion",
            "exportedAt",
            "futureKey"
        ]
    );
}

#[test]
fn document_text_containing_the_marker_still_unwraps() {
    // A payload whose bytes spell the marker (base64 hides it) and a source
    // name that does (the title is HTML-escaped, the meta JSON-escaped).
    let evil = format!("{PAYLOAD_OPEN}</script>");
    let html = wrap(
        &test_assets(),
        b"engine",
        "docx",
        &format!("{evil}.docx"),
        evil.as_bytes(),
        "0.5.0",
        WHEN,
    )
    .unwrap();
    let b = unwrap(&html).unwrap();
    assert_eq!(b.payload, evil.as_bytes());
    assert_eq!(b.meta.source_name(), format!("{evil}.docx"));
    assert_eq!(html.matches(PAYLOAD_OPEN).count(), 1);
}

#[test]
fn unsafe_assets_are_refused() {
    for bad in ["x = '</script>';", "/* </STYLE> */", "<!-- x"] {
        let mut assets = test_assets();
        assets.app_js = bad;
        assert!(
            matches!(
                wrap(&assets, b"", "docx", "a.docx", b"", "0", WHEN),
                Err(Error::UnsafeAsset(_))
            ),
            "{bad} must be refused"
        );
    }
}

#[test]
fn a_marker_spelled_in_script_text_does_not_confuse_readers() {
    // `rfind` lands on the real block, which is always last.
    let mut assets = test_assets();
    assets.engine_js =
        "var m = '<script type=\"application/x-docxy-payload\" id=\"docxy-payload\">';";
    let html = wrap(&assets, b"", "docx", "a.docx", b"real", "0", WHEN).unwrap();
    assert_eq!(unwrap(&html).unwrap().payload, b"real");
    let next = rewrap(&html, b"next").unwrap();
    assert!(next.contains(assets.engine_js));
    assert_eq!(unwrap(&next).unwrap().payload, b"next");
}

#[test]
fn data_blocks_are_inert() {
    let html = bundle(b"PK");
    // The ribbon's SVG markup and the shell template are escaped, so the only
    // `</script` sequences in the file are real closing tags.
    let closes = html.matches("</script>").count();
    let opens = html.matches("<script").count();
    assert_eq!(opens, closes);
    assert!(html.contains("\\u003csvg>"));
}

#[test]
fn slots_are_filled_in_one_pass() {
    // app.js contains a literal `{{css}}`: it must not be replaced by the CSS.
    let html = bundle(b"PK");
    assert!(html.contains("var APP = '{{css}}';"));
    assert_eq!(
        fill("a{{x}}b{{y}}c{{", &[("x", "{{y}}"), ("y", "Y")]),
        "a{{y}}bYc{{"
    );
}

#[test]
fn shell_block_holds_the_template() {
    let html = bundle(b"PK");
    let start = html.find("id=\"docxy-shell\">").unwrap() + "id=\"docxy-shell\">".len();
    let end = start + html[start..].find("</script>").unwrap();
    let shell_json = &html[start..end];
    let template = parse_string(&mut shell_json.chars().peekable()).unwrap();
    assert!(template.contains("<title>sample.docx</title>"));
    assert!(template.contains("{{payload}}") && template.contains("{{engine}}"));
    // Refilling the template from the element texts reproduces the file.
    let text_of = |id: &str| {
        let open = format!("id=\"{id}\">");
        let s = html.find(&open).unwrap() + open.len();
        let e = s + html[s..].find("</s").unwrap();
        html[s..e].to_string()
    };
    let rebuilt = fill(
        &template,
        &[
            ("shell", &text_of("docxy-shell")),
            ("css", &text_of("docxy-css")),
            ("ribbon", &text_of("docxy-ribbon")),
            ("engine", &text_of("docxy-engine")),
            ("engine_js", &text_of("docxy-engine-js")),
            ("app_js", &text_of("docxy-app-js")),
            ("payload", &text_of("docxy-payload")),
        ],
    );
    assert_eq!(rebuilt, html);
}

#[test]
fn the_shipped_web_ui_wraps() {
    // The real assets obey the raw-text rules (this is what fails if someone
    // writes `</script>` into app.js).
    let html = wrap(
        &docx_assets(),
        b"engine",
        "docx",
        "a.docx",
        b"PK",
        "0",
        WHEN,
    )
    .unwrap();
    assert_eq!(unwrap(&html).unwrap().payload, b"PK");
}

#[test]
fn meta_json_round_trips_awkward_text() {
    let mut meta = Meta::default();
    meta.set("sourceName", "a\"b\\c\n<d>&e\u{2028}\u{1F600}\u{7}.docx");
    let json = meta.to_json();
    assert!(!json.contains('<') && !json.contains('\n'));
    assert_eq!(Meta::from_json(&json).unwrap(), meta);
    assert_eq!(
        Meta::from_json(r#"{"k":"\ud83d\ude00"}"#).unwrap().get("k"),
        Some("\u{1F600}")
    );
    assert!(Meta::from_json(r#"{"k":1}"#).is_err());
    assert!(Meta::from_json(r#"{"k":"v"} x"#).is_err());
}

#[test]
fn names_follow_the_convention() {
    assert_eq!(bundle_name("sample.docx"), "sample.docx.html");
    assert_eq!(source_name("dir/sample.docx.html"), Some("dir/sample.docx"));
    assert_eq!(source_name("sample.html"), None);
    // Every HTML name is a bundle candidate, whatever its inner extension.
    for html in [
        r"C:\x\Report.DOCX.HTML",
        "sample.docx (1).html",
        "sample.docx(1).html",
        "notes.html",
        "old.HTM",
    ] {
        assert!(is_html_path(html), "{html}");
    }
    assert!(!is_html_path("sample.docx"));
    assert_eq!(bundle_inner_ext("book.xlsx.html").as_deref(), Some("xlsx"));
    assert_eq!(docx_source_name("notes.html"), "notes.docx");
    assert_eq!(docx_source_name("sample.docx.html"), "sample.docx");
    assert_eq!(docx_source_name("Report.DOCX.htm"), "Report.DOCX");
    assert_eq!(
        docx_source_name("sample.docx (1).html"),
        "sample.docx (1).docx"
    );
}

#[test]
fn a_slot_name_in_the_document_name_stays_text() {
    let html = wrap(
        &test_assets(),
        b"engine",
        "docx",
        "a{{payload}}b{{engine}}.docx",
        b"PK",
        "0",
        WHEN,
    )
    .unwrap();
    assert!(html.contains("<title>a&#123;&#123;payload}}b&#123;&#123;engine}}.docx</title>"));
    assert_eq!(html.matches(PAYLOAD_OPEN).count(), 1);
    let b = unwrap(&html).unwrap();
    assert_eq!(b.payload, b"PK");
    assert_eq!(b.meta.source_name(), "a{{payload}}b{{engine}}.docx");
}

/// The page rebuilds its own file in JS (`web/engine.js` `rebuildFile`), and
/// that must equal [`rewrap`] byte for byte. This writes the Rust side of that
/// comparison for `webapp/test/engine.test.mjs`: a bundle, a new payload, and
/// the rewrapped bundle. An awkward source name exercises the JSON escaping
/// both sides must share. Regenerate with `UPDATE_REWRAP_FIXTURE=1 cargo test
/// -p htmlbundle`.
#[test]
fn rewrap_fixture_for_the_page_is_current() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../webapp/test/fixtures");
    let before = wrap(
        &test_assets(),
        b"\0asm fake engine",
        "docx",
        "a \"quoted\" \\ <name> & {{payload}} \u{2028}\u{1F600}.docx",
        b"PK first payload",
        "0.5.0",
        WHEN,
    )
    .unwrap();
    let next: Vec<u8> = (0..=255u8).chain(*b"PK second payload").collect();
    let after = rewrap(&before, &next).unwrap();
    let files: [(&str, &[u8]); 3] = [
        ("rewrap-before.html", before.as_bytes()),
        ("rewrap-payload.bin", &next),
        ("rewrap-after.html", after.as_bytes()),
    ];
    if std::env::var_os("UPDATE_REWRAP_FIXTURE").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        return;
    }
    for (name, bytes) in files {
        let on_disk = std::fs::read(dir.join(name)).unwrap_or_default();
        assert!(
            on_disk == bytes,
            "webapp/test/fixtures/{name} is stale; regenerate with UPDATE_REWRAP_FIXTURE=1 cargo test -p htmlbundle"
        );
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("htmlbundle-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn sibling_warning_fires_only_when_the_original_changed() {
    let dir = temp_dir("sibling");
    let docx = dir.join("sample.docx");
    let bundle_path = dir.join("sample.docx.html");
    std::fs::write(&docx, b"original").unwrap();
    let html = bundle(b"original");
    std::fs::write(&bundle_path, &html).unwrap();
    let meta = unwrap(&html).unwrap().meta;
    assert_eq!(
        sibling_warning(&bundle_path, &meta),
        None,
        "untouched original"
    );

    // Editing the bundle does not make the untouched original look changed.
    let edited = unwrap(&rewrap(&html, b"edited in the browser").unwrap())
        .unwrap()
        .meta;
    assert_eq!(sibling_warning(&bundle_path, &edited), None);

    // Changing the original does, whatever the bundle is called.
    std::fs::write(&docx, b"original, edited in Word").unwrap();
    let w = sibling_warning(&bundle_path, &edited).unwrap();
    assert!(w.starts_with("sample.docx changed since export"), "{w}");
    let renamed = dir.join("sample.docx (1).html");
    assert!(sibling_warning(&renamed, &edited).is_some());
    // The recorded name cannot reach outside the folder.
    let mut sneaky = edited.clone();
    sneaky.set("sourceName", "../../sample.docx");
    let inner = dir.join("sub");
    std::fs::create_dir_all(&inner).unwrap();
    assert_eq!(sibling_warning(&inner.join("x.html"), &sneaky), None);
    // Nothing was written to the sibling.
    assert_eq!(std::fs::read(&docx).unwrap(), b"original, edited in Word");

    // No sibling, no warning.
    std::fs::remove_file(&docx).unwrap();
    assert_eq!(sibling_warning(&bundle_path, &edited), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timestamps() {
    assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc_timestamp(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(utc_timestamp(1_790_348_400), "2026-09-25T15:00:00Z");
}
