use super::lay::{self, Row, Run};
use super::*;
use crate::field_edit::Op;
use crate::help_manual::Source;
use crate::help_manual::tests::fixture_bundle;
use helpdocs::md::{Align, parse};

fn word() -> Arc<Bundle> {
    Arc::new(fixture_bundle(Editor::Word))
}

fn view(page: &str) -> HelpView {
    HelpView::new(word(), Loc::page(page), None)
}

fn at(v: &HelpView) -> (&str, Option<&str>) {
    (v.at.page.as_str(), v.at.anchor.as_deref())
}

/// A bundle of one page with `body`, and the fixture's image.
fn page_bundle(body: &str) -> Bundle {
    let png = std::fs::read(
        crate::help_manual::tests::fixture_dir(Editor::Word).join("images/ribbon.png"),
    )
    .unwrap();
    Bundle::from_files(
        Editor::Word,
        Source::BuiltIn,
        vec![
            ("contents.txt".into(), b"p\n".to_vec()),
            ("p.md".into(), body.as_bytes().to_vec()),
            ("images/ribbon.png".into(), png),
            ("images/bad.png".into(), b"not a picture".to_vec()),
        ],
    )
    .unwrap()
}

fn laid(body: &str) -> Vec<Row> {
    let b = page_bundle(body);
    lay::lay(&parse(body), &b)
}

// ---- navigation and history ------------------------------------------------

#[test]
fn a_view_opens_on_its_page_or_the_root() {
    let v = view("home");
    assert_eq!(at(&v), ("home", None));
    assert_eq!(v.title(), "Help: Home tab");
    assert_eq!((v.back.len(), v.forward.len()), (0, 0));
    assert_eq!(v.scroll_to.get(), Some(ScrollTo::Top));
    let v = view("nowhere");
    assert_eq!(at(&v), ("welcome", None));
    assert_eq!(v.title(), "Help: Welcome to docxy");
}

#[test]
fn back_and_forward_walk_the_history() {
    let mut v = view("welcome");
    v.navigate(Loc::page("home"), Some(Via::Area));
    v.navigate(Loc::page("tables"), None);
    assert_eq!(v.back, [Loc::page("welcome"), Loc::page("home")]);
    assert!(v.go_back());
    assert_eq!(at(&v), ("home", None));
    assert!(v.go_back());
    assert_eq!(at(&v), ("welcome", None));
    assert!(!v.go_back(), "nothing before the first page");
    assert_eq!(at(&v), ("welcome", None));
    assert!(v.go_forward());
    assert_eq!(at(&v), ("home", None));
    assert_eq!(v.forward, [Loc::page("tables")]);
    // A new navigation drops the forward list.
    v.navigate(Loc::page("keyboard"), None);
    assert!(v.forward.is_empty());
    assert!(!v.go_forward());
    assert_eq!(v.back, [Loc::page("welcome"), Loc::page("home")]);
}

#[test]
fn the_place_shown_is_no_history_step() {
    let mut v = view("home");
    v.scroll_to.set(None);
    v.navigate(Loc::page("home"), Some(Via::Area));
    assert!(v.back.is_empty());
    assert_eq!(v.via, Some(Via::Area));
    // It scrolls the page again: to its top, or to the heading asked for.
    assert_eq!(v.scroll_to.get(), Some(ScrollTo::Top));
    v.scroll_to.set(None);
    v.navigate(
        Loc {
            page: "home".into(),
            anchor: Some("clipboard".into()),
        },
        None,
    );
    assert_eq!(v.back, [Loc::page("home")], "another heading is a step");
    v.scroll_to.set(None);
    v.navigate(
        Loc {
            page: "home".into(),
            anchor: Some("clipboard".into()),
        },
        None,
    );
    assert_eq!(v.back, [Loc::page("home")], "the same heading is not");
    assert_eq!(v.scroll_to.get(), Some(ScrollTo::Block(3)));
    // An unknown page opens the root, as a step.
    v.navigate(Loc::page("nowhere"), None);
    assert_eq!(at(&v), ("welcome", None));
    assert_eq!(
        v.back.last(),
        Some(&Loc {
            page: "home".into(),
            anchor: Some("clipboard".into())
        })
    );
}

#[test]
fn an_anchor_scrolls_to_its_heading_block() {
    let mut v = view("welcome");
    v.navigate(
        Loc {
            page: "keyboard".into(),
            anchor: Some("shortcuts".into()),
        },
        None,
    );
    assert_eq!(at(&v), ("keyboard", Some("shortcuts")));
    // `# Keyboard` is block 0, `## Shortcuts` block 1.
    assert_eq!(v.anchor_block("shortcuts"), Some(1));
    assert_eq!(v.scroll_to.get(), Some(ScrollTo::Block(1)));
    // Back returns to the top of the page before.
    v.go_back();
    assert_eq!(v.scroll_to.get(), Some(ScrollTo::Top));
    // An anchor the page lacks scrolls to the top.
    v.navigate(
        Loc {
            page: "home".into(),
            anchor: Some("nothing".into()),
        },
        None,
    );
    assert_eq!(v.scroll_to.get(), Some(ScrollTo::Top));
}

// ---- links ------------------------------------------------------------------

#[test]
fn every_link_kind_follows() {
    let v = view("welcome");
    let page = |p: &str, a: Option<&str>| Link::Page {
        page: p.into(),
        anchor: a.map(str::to_string),
    };
    assert_eq!(
        v.follow(&page("home", None)),
        Follow::Navigate(Loc::page("home"))
    );
    assert_eq!(
        v.follow(&page("keyboard", Some("shortcuts"))),
        Follow::Navigate(Loc {
            page: "keyboard".into(),
            anchor: Some("shortcuts".into())
        })
    );
    assert_eq!(v.follow(&page("nowhere", None)), Follow::Nothing);
    assert_eq!(
        v.follow(&Link::Anchor("welcome".into())),
        Follow::Scroll("welcome".into())
    );
    assert_eq!(v.follow(&Link::Anchor("nowhere".into())), Follow::Nothing);
    assert_eq!(
        v.follow(&Link::External("https://example.com".into())),
        Follow::External("https://example.com".into())
    );
    assert_eq!(v.follow(&Link::Other("mailto:x".into())), Follow::Nothing);
}

#[test]
fn following_a_link_by_its_number_navigates_or_scrolls() {
    let mut v = view("welcome");
    // welcome: [Home tab](home), [keyboard shortcuts](keyboard#shortcuts),
    // [GitHub](https://github.com/yeroo/docxy).
    assert_eq!(v.links().len(), 3);
    assert_eq!(
        v.follow_link(2),
        Some(Follow::External("https://github.com/yeroo/docxy".into()))
    );
    assert_eq!(at(&v), ("welcome", None), "an external link stays");
    assert_eq!(v.follow_link(9), None);
    v.follow_link(1);
    assert_eq!(at(&v), ("keyboard", Some("shortcuts")));
    assert_eq!(v.back, [Loc::page("welcome")]);
    // images: [contents](#pictures), a heading of its own page.
    v.navigate(Loc::page("images"), None);
    assert_eq!(v.follow_link(0), Some(Follow::Scroll("pictures".into())));
    assert_eq!(at(&v), ("images", Some("pictures")));
    assert_eq!(v.scroll_to.get(), Some(ScrollTo::Block(0)));
    assert_eq!(v.back.last(), Some(&Loc::page("images")));
}

// ---- search -------------------------------------------------------------------

#[test]
fn typing_a_query_searches_live() {
    let mut v = view("welcome");
    for c in ["s", "t", "r", "i", "k", "e"] {
        v.edit_query(Op::Insert(c));
    }
    assert_eq!(v.query, "strike");
    assert!(v.hits.is_empty(), "every term must be whole");
    for c in "through".chars() {
        v.edit_query(Op::Insert(&c.to_string()));
    }
    assert_eq!(
        v.hits.iter().map(|h| h.page.as_str()).collect::<Vec<_>>(),
        ["home-font"]
    );
    // Enter opens the first hit.
    assert!(v.open_hit(0));
    assert_eq!(at(&v), ("home-font", None));
    assert!(!v.open_hit(5));
    // Escape clears the query, and the contents come back.
    v.clear_query();
    assert_eq!((v.query.as_str(), v.caret), ("", 0));
    assert!(v.hits.is_empty());
    // A blank query finds nothing.
    v.edit_query(Op::Insert("   "));
    assert!(v.hits.is_empty());
}

#[test]
fn the_search_box_edits_at_a_caret() {
    let mut v = view("welcome");
    v.edit_query(Op::Insert("tabls"));
    v.edit_query(Op::Left);
    v.edit_query(Op::Insert("e"));
    assert_eq!((v.query.as_str(), v.caret), ("tables", 5));
    assert!(v.hits.iter().any(|h| h.page == "tables"));
    v.edit_query(Op::SelectAll);
    v.edit_query(Op::Insert("bold"));
    assert_eq!(v.query, "bold");
}

// ---- help-read ---------------------------------------------------------------

#[test]
fn help_read_reports_the_page() {
    let mut v = view("welcome");
    v.navigate(Loc::page("home-font"), Some(Via::Exact));
    let j = v.read_json(0);
    assert_eq!(j.get_str("page"), Some("home-font"));
    assert_eq!(j.get_str("title"), Some("Font group"));
    assert_eq!(j.get_str("tab_title"), Some("Help: Font group"));
    assert_eq!(j.get_str("editor"), Some("word"));
    assert_eq!(j.get_str("via"), Some("exact"));
    assert_eq!(j.get("open").and_then(Json::as_bool), Some(true));
    assert_eq!(j.get_usize("back"), Some(1));
    assert_eq!(j.get_usize("forward"), Some(0));
    assert_eq!(j.get("anchor"), Some(&Json::Null));
    let headings = j.get("headings").and_then(Json::as_array).unwrap();
    assert_eq!(headings[0].get_str("slug"), Some("font-group"));
    assert_eq!(headings[0].get_usize("level"), Some(1));
    assert_eq!(headings[1].get_str("text"), Some("Effects"));
    let contents = j.get("contents").and_then(Json::as_array).unwrap();
    assert_eq!(contents[2].get_str("id"), Some("home-font"));
    assert_eq!(contents[2].get_usize("depth"), Some(1));
    assert_eq!(contents[2].get_str("title"), Some("Font group"));
    assert_eq!(j.get_str("top_heading"), Some("font-group"));
    assert_eq!(j.get_usize("problems"), Some(0));
    // The manual's revision, where it was read from and the footer (#1857):
    // the fixture is read from a directory, as an installed manual is.
    let fixture = "0000000000000000000000000000000000fixture";
    assert_eq!(j.get_str("revision"), Some(fixture));
    assert_eq!(j.get_str("source"), Some("installed"));
    assert_eq!(
        j.get_str("footer"),
        Some(format!("Word manual · revision {fixture} · installed").as_str())
    );

    let v = view("welcome");
    let j = v.read_json(0);
    let links = j.get("links").and_then(Json::as_array).unwrap();
    let got: Vec<_> = links
        .iter()
        .map(|l| {
            (
                l.get_usize("index").unwrap(),
                l.get_str("text").unwrap(),
                l.get_str("target").unwrap(),
                l.get_str("kind").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (0, "Home tab", "home", "page"),
            (1, "keyboard shortcuts", "keyboard#shortcuts", "page"),
            (2, "GitHub", "https://github.com/yeroo/docxy", "external"),
        ]
    );
    assert_eq!(j.get("via"), Some(&Json::Null));

    let v = view("images");
    let j = v.read_json(0);
    let images = j.get("images").and_then(Json::as_array).unwrap();
    assert_eq!(images[0].get_str("src"), Some("images/ribbon.png"));
    assert_eq!(images[0].get_str("alt"), Some("The ribbon"));
    assert_eq!(images[0].get("found"), Some(&Json::Bool(true)));
    let kinds: Vec<_> = j
        .get("links")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .map(|l| l.get_str("kind").unwrap())
        .collect();
    assert_eq!(kinds, ["anchor"]);
}

#[test]
fn help_read_reports_the_heading_at_the_top() {
    let v = view("keyboard");
    assert_eq!(v.read_json(0).get_str("top_heading"), Some("keyboard"));
    assert_eq!(v.read_json(1).get_str("top_heading"), Some("shortcuts"));
    assert_eq!(v.read_json(2).get_str("top_heading"), Some("shortcuts"));
    // The room below the last block reads as the last block.
    assert_eq!(v.read_json(99).get_usize("top_block"), Some(2));
}

/// A page of front matter only has no blocks: the read still answers.
#[test]
fn help_read_answers_on_a_page_without_blocks() {
    let b = Bundle::from_files(
        Editor::Word,
        Source::BuiltIn,
        vec![
            ("contents.txt".into(), b"empty\n".to_vec()),
            ("empty.md".into(), b"---\ntitle: Empty\n---\n".to_vec()),
        ],
    )
    .unwrap();
    assert!(b.manual.pages[0].blocks.is_empty());
    let v = HelpView::new(Arc::new(b), Loc::page("empty"), None);
    for top in [0, 1, 5] {
        let j = v.read_json(top);
        assert_eq!(j.get("top_heading"), Some(&Json::Null));
        assert_eq!(j.get_usize("top_block"), Some(0));
    }
    assert_eq!(v.title(), "Help: Empty");
}

#[test]
fn help_read_reports_the_search() {
    let mut v = view("welcome");
    v.edit_query(Op::Insert("bold"));
    let j = v.read_json(0);
    assert_eq!(j.get_str("query"), Some("bold"));
    let hits = j.get("hits").and_then(Json::as_array).unwrap();
    assert_eq!(hits[0].get_str("page"), Some("home-font"));
    assert!(hits[0].get_usize("score").unwrap() > 0);
}

// ---- the layout layer --------------------------------------------------------

#[test]
fn headings_take_their_level_size() {
    let rows = laid("# One\n## Two\n### Three\n#### Four\n");
    let sizes: Vec<_> = rows
        .iter()
        .map(|r| match r {
            Row::Heading {
                level, px, slug, ..
            } => (*level, *px, slug.as_str()),
            _ => panic!("{r:?}"),
        })
        .collect();
    assert_eq!(
        sizes,
        [
            (1, 24., "one"),
            (2, 20., "two"),
            (3, 17., "three"),
            (4, 15., "four")
        ]
    );
    assert!(lay::heading_px(4) > lay::BODY_PX);
}

#[test]
fn one_row_per_top_level_block() {
    let body = "# Title\n\nText.\n\n- a\n- b\n\n```\ncode\n```\n\n| x |\n|---|\n| 1 |\n\n![alt](images/ribbon.png)\n";
    let blocks = parse(body);
    let rows = laid(body);
    assert_eq!(rows.len(), blocks.len());
    assert!(matches!(rows[1], Row::Para(_)));
    assert!(matches!(rows[2], Row::List { depth: 0, .. }));
    assert!(matches!(rows[3], Row::Code { .. }));
    assert!(matches!(rows[4], Row::Table { cols: 1, .. }));
    assert!(matches!(rows[5], Row::Image { found: true, .. }));
}

#[test]
fn numbered_lists_count_from_their_start_and_nest_with_bullets() {
    let rows = laid("3. three\n4. four\n   - inner\n     - deeper\n\n   more text\n5. five\n");
    let Row::List { depth, items } = &rows[0] else {
        panic!("{rows:?}")
    };
    assert_eq!(*depth, 0);
    let markers: Vec<_> = items.iter().map(|i| i.marker.as_str()).collect();
    assert_eq!(markers, ["3.", "4.", "5."]);
    assert_eq!(lay::plain(&items[1].runs), "four");
    // The children of "four", in the order written.
    let kids = &items[1].children;
    let Row::List {
        depth,
        items: inner,
    } = &kids[0]
    else {
        panic!("{kids:?}")
    };
    assert_eq!((*depth, inner[0].marker.as_str()), (1, lay::bullet(1)));
    assert_eq!(lay::plain(&inner[0].runs), "inner");
    let Row::List {
        depth,
        items: deeper,
    } = &inner[0].children[0]
    else {
        panic!("{inner:?}")
    };
    assert_eq!((*depth, deeper[0].marker.as_str()), (2, lay::bullet(2)));
    assert!(
        matches!(kids.last(), Some(Row::Para(r)) if lay::plain(r) == "more text"),
        "{kids:?}"
    );
    let bullets = laid("- a\n- b\n");
    let Row::List { items, .. } = &bullets[0] else {
        panic!()
    };
    assert_eq!(items[0].marker, lay::bullet(0));
    assert_ne!(lay::bullet(0), lay::bullet(1));
}

#[test]
fn ragged_table_rows_are_padded_and_aligned_per_column() {
    let rows = laid("| A | B | C |\n|:---|:---:|---:|\n| 1 |\n| 1 | 2 | 3 |\n");
    let Row::Table {
        cols,
        align,
        header,
        rows,
    } = &rows[0]
    else {
        panic!("{rows:?}")
    };
    assert_eq!(*cols, 3);
    assert_eq!(align, &[Align::Left, Align::Center, Align::Right]);
    assert_eq!(header.len(), 3);
    assert_eq!(lay::plain(&header[1]), "B");
    assert!(rows.iter().all(|r| r.len() == 3));
    assert_eq!(lay::plain(&rows[0][0]), "1");
    assert!(rows[0][2].is_empty(), "padded");
    // A row wider than the header widens the table.
    let wide = laid("| A |\n|---|\n| 1 | 2 |\n");
    let Row::Table {
        cols,
        align,
        header,
        ..
    } = &wide[0]
    else {
        panic!()
    };
    assert_eq!((*cols, align.len(), header.len()), (2, 2, 2));
    assert_eq!(align[1], Align::None);
}

#[test]
fn images_are_found_only_when_the_bundle_holds_a_picture() {
    let rows =
        laid("![ok](images/ribbon.png)\n\n![gone](images/none.png)\n\n![junk](images/bad.png)\n");
    let found: Vec<_> = rows
        .iter()
        .map(|r| match r {
            Row::Image { alt, found, .. } => (alt.as_str(), *found),
            _ => panic!("{r:?}"),
        })
        .collect();
    assert_eq!(found, [("ok", true), ("gone", false), ("junk", false)]);
}

#[test]
fn inline_runs_carry_their_look() {
    let rows = laid("Plain **bold *both*** and `code` *it*.\n");
    let Row::Para(runs) = &rows[0] else { panic!() };
    let looks: Vec<_> = runs
        .iter()
        .map(|r| match r {
            Run::Text {
                text,
                bold,
                italic,
                code,
                ..
            } => (text.as_str(), *bold, *italic, *code),
            _ => panic!("{r:?}"),
        })
        .collect();
    assert_eq!(
        looks,
        [
            ("Plain ", false, false, false),
            ("bold ", true, false, false),
            ("both", true, true, false),
            (" and ", false, false, false),
            ("code", false, false, true),
            (" ", false, false, false),
            ("it", false, true, false),
            (".", false, false, false),
        ]
    );
}

#[test]
fn key_chords_are_key_caps() {
    let rows = laid("Press <kbd>Ctrl+Shift+L</kbd> now.\n");
    let Row::Para(runs) = &rows[0] else { panic!() };
    assert_eq!(
        runs[1],
        Run::Keys {
            caps: vec!["Ctrl".into(), "Shift".into(), "L".into()],
            link: None
        }
    );
    assert_eq!(lay::plain(runs), "Press Ctrl+Shift+L now.");
}

#[test]
fn link_runs_are_numbered_in_the_pages_link_order() {
    // Links in a heading, a list item and its child, a table and a
    // paragraph, with a key chord inside a link.
    let body = "# See [a](a)\n\n- [b](b)\n  - [c](#c) and [d](https://d.example)\n\n| [e](e) | x |\n|---|---|\n| [<kbd>F1</kbd>](f) | |\n\nLast [g](g).\n";
    let blocks = parse(body);
    let rows = laid(body);
    let mut numbered = Vec::new();
    fn push(runs: &[Run], out: &mut Vec<(usize, String)>) {
        for r in runs {
            if let Some(n) = r.link() {
                let text = lay::plain(std::slice::from_ref(r));
                match out.last_mut() {
                    Some((m, t)) if *m == n => t.push_str(&text),
                    _ => out.push((n, text)),
                }
            }
        }
    }
    fn walk(rows: &[Row], out: &mut Vec<(usize, String)>) {
        for r in rows {
            match r {
                Row::Heading { runs, .. } | Row::Para(runs) => push(runs, out),
                Row::List { items, .. } => {
                    for i in items {
                        push(&i.runs, out);
                        walk(&i.children, out);
                    }
                }
                Row::Table { header, rows, .. } => {
                    for c in header.iter().chain(rows.iter().flatten()) {
                        push(c, out);
                    }
                }
                Row::Code { .. } | Row::Image { .. } => {}
            }
        }
    }
    walk(&rows, &mut numbered);
    let ids: Vec<usize> = numbered.iter().map(|(n, _)| *n).collect();
    assert_eq!(ids, (0..7).collect::<Vec<_>>());
    let texts: Vec<_> = numbered.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(texts, ["a", "b", "c", "d", "e", "F1", "g"]);
    // The same order as the page's links, which `help-read` numbers.
    let targets: Vec<_> = md::links(&blocks)
        .into_iter()
        .map(|l| link_kind(l).1)
        .collect();
    assert_eq!(
        targets,
        ["a", "b", "#c", "https://d.example", "e", "f", "g"]
    );
}

#[test]
fn code_keeps_its_lines() {
    let rows = laid("```rust\nfn a() {}\n  b\n```\n");
    assert_eq!(
        rows[0],
        Row::Code {
            lang: "rust".into(),
            lines: vec!["fn a() {}".into(), "  b".into()]
        }
    );
}

/// The footer names the manual, its revision (a commit cut to 7) and where
/// it was read from; a bundle without a REVISION says so (#1857).
#[test]
fn the_footer_names_the_manual_its_revision_and_source() {
    let bundle = |editor: Editor, source: Source, revision: Option<&str>| {
        let mut files = vec![
            ("contents.txt".to_string(), b"p\n".to_vec()),
            ("p.md".to_string(), b"# P\n".to_vec()),
        ];
        if let Some(r) = revision {
            files.push(("REVISION".into(), r.as_bytes().to_vec()));
        }
        Bundle::from_files(editor, source, files).unwrap()
    };
    let sha = "0123abcdef0123abcdef0123abcdef0123abcdef\n";
    assert_eq!(
        footer_text(&bundle(Editor::Word, Source::BuiltIn, Some(sha))),
        "Word manual · revision 0123abc · built in"
    );
    assert_eq!(
        footer_text(&bundle(Editor::Excel, Source::Installed, Some("v2026.11"))),
        "Excel manual · revision v2026.11 · installed"
    );
    assert_eq!(
        footer_text(&bundle(Editor::Project, Source::BuiltIn, None)),
        "Project manual · revision unknown · built in"
    );
    assert_eq!(
        footer_text(&bundle(Editor::Word, Source::Installed, Some("\n"))),
        "Word manual · revision unknown · installed"
    );
    // help-read reports what the footer draws, and no revision as null.
    let v = HelpView::new(
        Arc::new(bundle(Editor::Word, Source::BuiltIn, None)),
        Loc::page("p"),
        None,
    );
    let j = v.read_json(0);
    assert_eq!(j.get("revision"), Some(&Json::Null));
    assert_eq!(j.get_str("source"), Some("built-in"));
    assert_eq!(
        j.get_str("footer"),
        Some("Word manual · revision unknown · built in")
    );
}
