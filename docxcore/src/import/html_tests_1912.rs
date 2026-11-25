//! The links leftovers of #1879 (#1912): bookmarks for `<a name>` and the
//! `id`s a page links to, Word's `Hyperlink` character style, relative
//! links resolved against the page's base, a link's `title` and `target`.

use super::*;
use crate::model::{Block as DocBlock, Hyperlink, Paragraph};

const U: &str = "https://x.test/";

fn doc(html: &str) -> Document {
    import_html(html.as_bytes()).unwrap()
}

fn paras(d: &Document) -> Vec<Paragraph> {
    fn walk(blocks: &[DocBlock], out: &mut Vec<Paragraph>) {
        for b in blocks {
            match b {
                DocBlock::Paragraph(p) => out.push(p.clone()),
                DocBlock::Table(t) => {
                    for c in t.rows.iter().flat_map(|r| &r.cells) {
                        walk(&c.blocks, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&d.body, &mut out);
    out
}

fn links(d: &Document) -> Vec<Hyperlink> {
    paras(d)
        .iter()
        .flat_map(|p| p.content.clone())
        .filter_map(|i| match i {
            Inline::Hyperlink(h) => Some(h),
            _ => None,
        })
        .collect()
}

/// One paragraph's inlines as a sketch: `Mark <name>`, `End`, `Run "x"`,
/// `Link <target or #anchor> ["a"]`, `Break`, `Tab`, `Object`.
fn sketch(p: &Paragraph) -> Vec<String> {
    p.content
        .iter()
        .map(|i| match i {
            Inline::Raw(r) if r.starts_with("<w:bookmarkStart ") => {
                let name = r.split("w:name=\"").nth(1).unwrap();
                format!("Mark {}", name.trim_end_matches("\"/>"))
            }
            Inline::Raw(r) if r.starts_with("<w:bookmarkEnd ") => "End".into(),
            Inline::Run(r) => format!("Run {:?}", r.text),
            Inline::Hyperlink(h) => {
                let to = match (&h.target, &h.anchor) {
                    (Some(t), None) => t.clone(),
                    (None, Some(a)) => format!("#{a}"),
                    other => format!("{other:?}"),
                };
                let runs: Vec<&str> = h.runs.iter().map(|r| r.text.as_str()).collect();
                format!("Link {to} {runs:?}")
            }
            Inline::Break(..) => "Break".into(),
            Inline::Tab(_) => "Tab".into(),
            _ => "Object".into(),
        })
        .collect()
}

fn sketches(d: &Document) -> Vec<Vec<String>> {
    paras(d).iter().map(sketch).collect()
}

/// Every bookmark name in the document, in order.
fn marks(d: &Document) -> Vec<String> {
    sketches(d)
        .into_iter()
        .flatten()
        .filter_map(|s| s.strip_prefix("Mark ").map(str::to_string))
        .collect()
}

/// `import` packaged, saved and loaded again: the document part's XML,
/// the rels part's and the reloaded document.
fn saved(import: HtmlImport) -> (String, String, Document) {
    let bytes = crate::package::save_package(&import.package());
    let pkg = crate::package::load_package(&bytes).expect("reloads");
    let xml = pkg.part_text("word/document.xml").unwrap();
    let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
    (xml, rels, pkg.document)
}

fn import_plain(html: &str) -> HtmlImport {
    import_html_within(html.as_bytes(), &Budget::standard(), None).unwrap()
}

// --- Bookmarks (plan-p1) ---

#[test]
fn an_anchor_link_jumps_to_the_bookmark_its_id_makes_1912() {
    let page = r##"<p><a href="#s2">Go</a></p><h2 id="s2">Two</h2>"##;
    let d = doc(page);
    assert_eq!(
        sketches(&d),
        [
            vec![r#"Link #s2 ["Go"]"#.to_string()],
            vec!["Mark s2".into(), "End".into(), r#"Run "Two""#.into()]
        ]
    );
    // A save keeps both, and they still meet after a reload.
    let (xml, _, back) = saved(import_plain(page));
    assert!(xml.contains(r#"<w:hyperlink w:anchor="s2">"#), "{xml}");
    assert!(
        xml.contains(r#"<w:bookmarkStart w:id="0" w:name="s2"/><w:bookmarkEnd w:id="0"/>"#),
        "{xml}"
    );
    assert_eq!(links(&back)[0].anchor.as_deref(), Some("s2"));
    assert_eq!(marks(&back), ["s2"]);
}

#[test]
fn a_link_and_its_target_go_through_one_name_mapping_1912() {
    for (href, id) in [
        ("#my%20section", "my section"),
        ("#my section", "my section"),
        ("#a-b.c", "a-b.c"),
    ] {
        let d = doc(&format!("<p><a href='{href}'>go</a></p><p id='{id}'>T</p>"));
        let want = bookmark_name(id);
        assert_eq!(
            links(&d)[0].anchor.as_deref(),
            Some(want.as_str()),
            "{href}"
        );
        assert_eq!(marks(&d), [want], "{href}");
    }
    // A table of contents after its sections finds them too.
    let d = doc("<h2 id=a>A</h2><p><a href=#a>back</a></p>");
    assert_eq!(marks(&d), ["a"]);
}

#[test]
fn bookmark_names_follow_words_rule_1912() {
    assert_eq!(bookmark_name("my section"), "my_section");
    assert_eq!(bookmark_name("a-b.c"), "a_b_c");
    // A digit-led name is a hidden one; one led by `_` is kept.
    assert_eq!(bookmark_name("1intro"), "_1intro");
    // Any other leading character is a `_` already.
    assert_eq!(bookmark_name("-x"), "_x");
    assert_eq!(bookmark_name("_MailEndCompose"), "_MailEndCompose");
    // Letters of any script stay.
    assert_eq!(bookmark_name("café"), "café");
    assert_eq!(bookmark_name("раздел 2"), "раздел_2");
    // Cut to 40 characters, not bytes.
    assert_eq!(bookmark_name(&"é".repeat(50)), "é".repeat(40));
    assert_eq!(bookmark_name(&"9".repeat(50)).chars().count(), 40);
    assert_eq!(bookmark_name(""), "");
}

#[test]
fn an_a_name_is_always_a_bookmark_and_an_unlinked_id_is_none_1912() {
    let d = doc("<p><a name=top></a>Top</p><p id=x>X</p><h2 id=y>Y</h2>");
    assert_eq!(
        sketches(&d),
        [
            vec!["Mark top".to_string(), "End".into(), r#"Run "Top""#.into()],
            vec![r#"Run "X""#.to_string()],
            vec![r#"Run "Y""#.to_string()],
        ]
    );
    // Outlook's own marks keep their names.
    let d = doc("<p><a name=\"_MailEndCompose\">x</a></p>");
    assert_eq!(marks(&d), ["_MailEndCompose"]);
    // An `a` with an `id` follows the `id` rule.
    let d = doc("<p><a id=z>z</a></p>");
    assert!(marks(&d).is_empty());
}

#[test]
fn a_bookmark_makes_and_keeps_no_paragraph_of_its_own_1912() {
    // An empty linked block: the mark goes to the next content.
    let linked = doc("<p><a href=#x>go</a></p><div id=x></div><p>T</p>");
    let plain = doc("<p><a href=#x>go</a></p><div></div><p>T</p>");
    assert_eq!(paras(&linked).len(), paras(&plain).len());
    assert_eq!(sketch(&paras(&linked)[1]), ["Mark x", "End", r#"Run "T""#]);
    // A lone `<br>` is its block's one line, a mark in front or not.
    let linked = doc("<p><a href=#x>go</a></p><div id=x><br></div><p>T</p>");
    let plain = doc("<p><a href=#x>go</a></p><div><br></div><p>T</p>");
    let strip = |d: &Document| -> Vec<Vec<String>> {
        sketches(d)
            .into_iter()
            .map(|p| {
                p.into_iter()
                    .filter(|s| !s.starts_with("Mark") && s != "End")
                    .collect()
            })
            .collect()
    };
    assert_eq!(strip(&linked), strip(&plain));
    // The block's line is the empty paragraph a lone `<br>` makes; the
    // mark goes on to the next content.
    assert_eq!(
        sketches(&linked),
        [
            vec![r#"Link #x ["go"]"#.to_string()],
            vec![],
            vec!["Mark x".into(), "End".into(), r#"Run "T""#.into()]
        ]
    );
    // A block of only white space is the empty paragraph it was; its
    // mark moves on.
    let linked = doc("<p><a href=#x>go</a></p><p id=x>&nbsp;</p><p>T</p>");
    let plain = doc("<p><a href=#x>go</a></p><p>&nbsp;</p><p>T</p>");
    assert_eq!(strip(&linked), strip(&plain));
    assert_eq!(sketch(&paras(&linked)[1]), Vec::<String>::new());
    assert_eq!(sketch(&paras(&linked)[2]), ["Mark x", "End", r#"Run "T""#]);
}

#[test]
fn the_first_of_two_targets_with_one_name_wins_1912() {
    let d = doc("<p><a href=#a>go</a></p><p id=a>1</p><p id=a>2</p><p id='a b'>3</p>");
    assert_eq!(marks(&d), ["a"]);
    let d = doc("<p><a href='#a b'>go</a></p><p id='a b'>1</p><p id=a_b>2</p>");
    assert_eq!(marks(&d), ["a_b"]);
    assert_eq!(sketch(&paras(&d)[1])[0], "Mark a_b");
}

#[test]
fn a_bookmark_in_a_table_cell_is_in_the_cell_1912() {
    let d = doc("<p><a href=#c>go</a></p><table><tr><td id=c>cell</td></tr></table>");
    let DocBlock::Table(t) = &d.body[1] else {
        panic!("{:?}", d.body)
    };
    let DocBlock::Paragraph(p) = &t.rows[0].cells[0].blocks[0] else {
        panic!()
    };
    assert_eq!(sketch(p), ["Mark c", "End", r#"Run "cell""#]);
}

#[test]
fn a_bookmark_left_at_the_end_ends_the_last_paragraph_1912() {
    let d = doc("<p>T</p><a name=end></a>");
    assert_eq!(sketches(&d), [vec![r#"Run "T""#, "Mark end", "End"]]);
    // After a table: dropped, no empty paragraph made.
    let d = doc("<table><tr><td>c</td></tr></table><a name=end></a>");
    assert!(
        matches!(
            &d.body[..],
            [DocBlock::Table(_), DocBlock::SectionProperties(_)]
        ),
        "{:?}",
        d.body
    );
    assert!(marks(&d).is_empty());
}

#[test]
fn a_clipboard_paste_makes_no_bookmarks_and_no_link_style_1912() {
    let page = "<p><a name=n></a>A <a href=#x>go</a> <a href=https://x.test/>web</a></p>\
                <p id=x>T</p>";
    let clip = crate::editor::Clip::from_html(page, None).expect("a clip");
    for inline in clip.paras.iter().flatten() {
        match inline {
            Inline::Raw(r) => panic!("a raw in a paste: {r}"),
            Inline::Hyperlink(h) => {
                assert!(h.runs.iter().all(|r| r.props.style_id.is_none()), "{h:?}");
            }
            _ => {}
        }
    }
    // Its anchor link is still one, through the mapping.
    let anchors: Vec<_> = clip
        .paras
        .iter()
        .flatten()
        .filter_map(|i| match i {
            Inline::Hyperlink(h) => h.anchor.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(anchors, ["x"]);
}

// --- The Hyperlink style (plan-p2) ---

#[test]
fn link_text_in_its_link_look_takes_the_hyperlink_style_1912() {
    let styled = |page: &str| -> Vec<Option<String>> {
        links(&doc(page))
            .iter()
            .flat_map(|h| h.runs.iter().map(|r| r.props.style_id.clone()))
            .collect()
    };
    let link = Some("Hyperlink".to_string());
    assert_eq!(
        styled(&format!("<p><a href={U}>x</a></p>")),
        std::slice::from_ref(&link)
    );
    // A page colour, still underlined.
    assert_eq!(
        styled(&format!("<p><a href={U} style='color:red'>x</a></p>")),
        std::slice::from_ref(&link)
    );
    // Restyled to no underline or no colour: none, as before.
    assert_eq!(
        styled(&format!(
            "<p><a href={U} style='text-decoration:none'>x</a></p>"
        )),
        [None]
    );
    assert_eq!(
        styled(&format!("<p><a href={U} style='color:inherit'>x</a></p>")),
        [None]
    );
    // An anchor link takes it too; text outside a link never does.
    let d = doc("<p>a <a href=#s>b</a></p>");
    let Inline::Run(r) = &paras(&d)[0].content[0] else {
        panic!()
    };
    assert_eq!(r.props.style_id, None);
    assert_eq!(links(&d)[0].runs[0].props.style_id, link);
}

#[test]
fn the_hyperlink_style_changes_nothing_drawn_1912() {
    let import = import_plain(&format!("<p><a href={U}>x</a></p>"));
    let run = links(&import.document)[0].runs[0].clone();
    let pkg = import.package();
    let styles = pkg.part_text("word/styles.xml").unwrap();
    assert!(
        styles.contains(
            "<w:style w:type=\"character\" w:styleId=\"Hyperlink\">\
             <w:name w:val=\"Hyperlink\"/><w:uiPriority w:val=\"99\"/><w:unhideWhenUsed/>\
             <w:rPr><w:color w:val=\"0563C1\"/><w:u w:val=\"single\"/></w:rPr></w:style>"
        ),
        "{styles}"
    );
    let sheet = crate::styles::parse_styles_xml(&styles);
    let shown = sheet.shown_run(None, None, run.props.style_id.as_deref(), &run.props);
    assert_eq!(shown, run.props);
    let unstyled = RunProps {
        style_id: None,
        ..run.props.clone()
    };
    assert_eq!(
        sheet.shown_run(None, None, None, &unstyled).underline,
        shown.underline
    );
    assert_eq!(
        sheet.shown_run(None, None, None, &unstyled).color,
        shown.color
    );
    // Saved, the run names the style.
    let (xml, _, _) = saved(import_plain(&format!("<p><a href={U}>x</a></p>")));
    assert!(xml.contains("<w:rStyle w:val=\"Hyperlink\"/>"), "{xml}");
}

// --- Relative links (plan-p3) ---

#[test]
fn a_relative_link_resolves_against_the_base_href_1912() {
    let target = |base: &str, href: &str| -> Option<String> {
        let d = doc(&format!("{base}<p><a href='{href}'>t</a></p>"));
        links(&d).first().and_then(|h| h.target.clone())
    };
    let base = "<base href='https://x.test/d/i.html'>";
    for (href, want) in [
        ("p.html", "https://x.test/d/p.html"),
        ("../p.html", "https://x.test/p.html"),
        ("/x", "https://x.test/x"),
        ("//h.test/x", "https://h.test/x"),
        ("?q=1", "https://x.test/d/i.html?q=1"),
        ("a b.html", "https://x.test/d/a%20b.html"),
        // An absolute one is itself.
        ("http://y.test/", "http://y.test/"),
    ] {
        assert_eq!(target(base, href).as_deref(), Some(want), "{href}");
    }
    // `#name` stays an anchor link whatever the base.
    let d = doc(&format!("{base}<p><a href=#top>t</a></p>"));
    assert_eq!(links(&d)[0].anchor.as_deref(), Some("top"));
    // A resolved scheme outside the allow-list, or none, is no link.
    for (base, href) in [
        ("", "p.html"),
        ("<base href='d/'>", "p.html"),
        ("<base href='mailto:a@b.test'>", "x"),
        ("<base href='cid:part1'>", "x"),
        ("<base href='https:x.test/'>", "x"),
        ("<base href='file:///c:/d/'>", "p.html"),
        (base, "javascript:alert(1)"),
        (base, ""),
    ] {
        assert_eq!(target(base, href), None, "{base} {href}");
    }
    // An ftp base too.
    assert_eq!(
        target("<base href='ftp://f.test/pub/'>", "a.txt").as_deref(),
        Some("ftp://f.test/pub/a.txt")
    );
}

#[test]
fn a_relative_link_resolves_against_an_mhtml_roots_location_1912() {
    let page = "<p><a href='p.html'>t</a></p>";
    let got = import_decoded(
        page,
        &Budget::standard(),
        None,
        None,
        Some("https://m.test/a/root.html"),
        true,
    )
    .unwrap();
    assert_eq!(
        links(&got.document)[0].target.as_deref(),
        Some("https://m.test/a/p.html")
    );
    // A relative `<base>` resolves against the location first.
    let page = "<base href='sub/'><p><a href='p.html'>t</a></p>";
    let got = import_decoded(
        page,
        &Budget::standard(),
        None,
        None,
        Some("https://m.test/a/root.html"),
        true,
    )
    .unwrap();
    assert_eq!(
        links(&got.document)[0].target.as_deref(),
        Some("https://m.test/a/sub/p.html")
    );
}

// --- Title and target (plan-p4) ---

#[test]
fn a_links_title_and_target_are_its_tooltip_and_frame_1912() {
    let d = doc(&format!(
        "<p><a href={U} title=' Go there ' target=_blank>u</a> \
         <a href=#s title='a &quot;b&quot; &lt;c&gt;'>s</a> <a href={U}>p</a></p>"
    ));
    let ls = links(&d);
    assert_eq!(
        ls[0].raw.as_deref(),
        Some(r#"<w:hyperlink w:tooltip="Go there" w:tgtFrame="_blank"/>"#)
    );
    assert!(ls[0].content_changed);
    assert_eq!(ls[0].target.as_deref(), Some(U));
    assert_eq!(
        ls[1].raw.as_deref(),
        Some(r#"<w:hyperlink w:anchor="s" w:tooltip="a &quot;b&quot; &lt;c&gt;"/>"#)
    );
    // Neither: the link it always was.
    assert_eq!(ls[2].raw, None);
    assert!(!ls[2].content_changed);
}

#[test]
fn a_title_and_a_target_are_read_as_word_keeps_them_1912() {
    let opener = |attrs: &str| -> Option<String> {
        let d = doc(&format!("<p><a href={U} {attrs}>u</a></p>"));
        links(&d)[0].raw.clone()
    };
    // Empty, white space, `_self` (any case), a control character, a
    // target over 255 characters: none.
    for attrs in [
        "title=''",
        "title='  '",
        "target=''",
        "target=_self",
        "target=_SELF",
        "target='a&#1;b'",
    ] {
        assert_eq!(opener(attrs), None, "{attrs}");
    }
    let long = "f".repeat(MAX_LINK_ATTR + 1);
    assert_eq!(opener(&format!("target={long}")), None);
    let at = "f".repeat(MAX_LINK_ATTR);
    assert_eq!(
        opener(&format!("target={at}")),
        Some(format!(r#"<w:hyperlink w:tgtFrame="{at}"/>"#))
    );
    // A title: characters XML cannot hold dropped, cut to 255.
    assert_eq!(
        opener("title='a&#1;b'").as_deref(),
        Some(r#"<w:hyperlink w:tooltip="ab"/>"#)
    );
    let long = "t".repeat(300);
    assert_eq!(
        opener(&format!("title={long}")),
        Some(format!(
            r#"<w:hyperlink w:tooltip="{}"/>"#,
            "t".repeat(MAX_LINK_ATTR)
        ))
    );
}

#[test]
fn links_join_only_with_the_same_title_and_target_1912() {
    let d = doc(&format!(
        "<p><a href={U} title=t>a</a><a href={U} title=t><i>b</i></a>\
         <a href={U} title=u>c</a><a href={U}>d</a><a href={U} target=w>e</a></p>"
    ));
    let ls = links(&d);
    let texts: Vec<Vec<&str>> = ls
        .iter()
        .map(|h| h.runs.iter().map(|r| r.text.as_str()).collect())
        .collect();
    assert_eq!(texts, [vec!["a", "b"], vec!["c"], vec!["d"], vec!["e"]]);
    // A titled link's continuation after a break is another link with
    // its title.
    let d = doc(&format!("<p><a href={U} title=t>a<br>b c</a></p>"));
    let ls = links(&d);
    assert_eq!(ls.len(), 2);
    assert_eq!(ls[0].raw, ls[1].raw);
    assert_eq!(ls[1].runs[0].text, "b c");
}

#[test]
fn a_saved_link_keeps_its_tooltip_frame_and_url_1912() {
    let (xml, rels, back) = saved(import_plain(&format!(
        "<p><a href={U} title=Tip target=_blank>u</a> <a href=#s title=T2>s</a></p><p id=s>S</p>"
    )));
    let rid = rels
        .split("<Relationship ")
        .find(|r| r.contains(U))
        .and_then(|r| r.split("Id=\"").nth(1))
        .and_then(|r| r.split('"').next())
        .expect("a relationship for the URL");
    assert!(
        xml.contains(&format!(
            r#"<w:hyperlink r:id="{rid}" w:tooltip="Tip" w:tgtFrame="_blank">"#
        )),
        "{xml}"
    );
    assert!(
        xml.contains(r#"<w:hyperlink w:anchor="s" w:tooltip="T2">"#),
        "{xml}"
    );
    let ls = links(&back);
    assert_eq!(ls[0].target.as_deref(), Some(U));
    assert_eq!(ls[0].runs[0].text, "u");
    assert_eq!(ls[1].anchor.as_deref(), Some("s"));
}

#[test]
fn a_picture_in_a_titled_link_takes_its_title_as_tooltip_1912() {
    let mut resolve =
        |src: &str, _: usize| (src == "logo.png").then(|| super::tests_1898::png(1, 1));
    let page = format!(
        "<p><a href={U} title='say &quot;hi&quot; r:id=&quot;evil&quot; it&apos;s'>\
         <img src=logo.png width=10 height=10></a></p>\
         <p><a href=https://b.test/><img src=logo.png width=10 height=10></a></p>"
    );
    let got = import_html_within(page.as_bytes(), &Budget::standard(), Some(&mut resolve)).unwrap();
    let pics = super::tests_1898::pictures(&got.document.body)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(
        pics[0].contains(
            "r:id=\"rIdHtmlLink1\" tooltip=\"say &quot;hi&quot; r:id=&quot;evil&quot; it&apos;s\"/>"
        ),
        "{}",
        pics[0]
    );
    assert_eq!(
        crate::image::inline_click_link(&pics[0]).as_deref(),
        Some("rIdHtmlLink1")
    );
    // No title, no tooltip.
    assert!(!pics[1].contains("tooltip"), "{}", pics[1]);
    // The package renames the real id, the title left alone.
    let pkg = got.package();
    let xml = crate::serialize::document_to_xml(&pkg.document);
    assert!(!xml.contains("rIdHtmlLink"), "{xml}");
    assert!(xml.contains("r:id=&quot;evil&quot;"), "{xml}");
    let rid = crate::image::inline_click_link(super::tests_1898::pictures(&pkg.document.body)[0])
        .unwrap();
    let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
    assert!(
        rels.contains(&format!("Id=\"{rid}\"")) && rels.contains(U),
        "{rid}: {rels}"
    );
}
