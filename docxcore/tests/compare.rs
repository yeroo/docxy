//! Review > Compare: the result's tracked changes turn the original into the
//! revised document (#626).

use docxcore::compare::{CompareOptions, CompareResult, CompareSkip, compare_packages};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, RevisionCategory, RevisionKind, UnsupportedRevisionKind};
use docxcore::package::{Package, load_package, new_markdown_package, new_package, save_package};
use docxcore::review::RevisionOutcome;
use docxcore::serialize::document_to_xml;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const DATE: &str = "2026-10-02T12:00:00Z";

fn doc(body: &str) -> Document {
    parse_document_xml(
        &format!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\"><w:body>{body}</w:body></w:document>"
        ),
        &Relationships::default(),
    )
}

fn pkg(body: &str) -> Package {
    new_package(doc(body))
}

fn p(text: &str) -> String {
    if text.is_empty() {
        return "<w:p/>".to_string();
    }
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

fn paras(texts: &[&str]) -> String {
    texts.iter().map(|t| p(t)).collect()
}

fn compare(original: &Package, revised: &Package) -> CompareResult {
    compare_packages(
        original,
        revised,
        &CompareOptions {
            author: "Tester".to_string(),
            date: DATE.to_string(),
        },
    )
}

/// Paragraph texts of a container, descending into table cells.
fn texts_of(blocks: &[Block], out: &mut Vec<String>) {
    for block in blocks {
        match block {
            Block::Paragraph(p) => out.push(p.plain_text()),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        texts_of(&cell.blocks, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn texts(document: &Document) -> Vec<String> {
    let mut out = Vec::new();
    texts_of(&document.body, &mut out);
    out
}

fn resolved(document: &Document, accept: bool) -> Document {
    let mut document = document.clone();
    let outcomes = if accept {
        document.accept_all_revisions()
    } else {
        document.reject_all_revisions()
    };
    assert!(
        outcomes.iter().all(RevisionOutcome::is_applied),
        "{outcomes:?}"
    );
    assert!(document.revisions().is_empty());
    document
}

fn reloaded(result: &CompareResult) -> Document {
    load_package(&save_package(&result.package))
        .expect("result reloads")
        .document
}

/// Accept-all gives the revised paragraphs and reject-all the original ones,
/// in memory and after a save/load round trip.
fn assert_round_trips(original: &[&str], revised: &[&str]) {
    let result = compare(&pkg(&paras(original)), &pkg(&paras(revised)));
    for (label, document) in [
        ("in memory", result.package.document.clone()),
        ("reloaded", reloaded(&result)),
    ] {
        assert_eq!(
            texts(&resolved(&document, true)),
            revised,
            "accept-all {label}: {original:?} -> {revised:?}\n{}",
            document_to_xml(&document)
        );
        assert_eq!(
            texts(&resolved(&document, false)),
            original,
            "reject-all {label}: {original:?} -> {revised:?}\n{}",
            document_to_xml(&document)
        );
    }
}

#[test]
fn the_issue_example_marks_words_and_resolves_both_ways() {
    let result = compare(
        &pkg(&p("The cat sat on the mat.")),
        &pkg(&p("The black cat sat on a mat.")),
    );
    let document = &result.package.document;
    assert_eq!(
        texts(&resolved(document, true)),
        ["The black cat sat on a mat."]
    );
    assert_eq!(
        texts(&resolved(document, false)),
        ["The cat sat on the mat."]
    );
    assert_eq!((result.insertions, result.deletions), (2, 1));
    assert!(result.skipped.is_empty());

    let xml = document_to_xml(document);
    assert!(xml.contains("<w:ins w:id="), "{xml}");
    assert!(xml.contains("w:author=\"Tester\""), "{xml}");
    assert!(xml.contains(&format!("w:date=\"{DATE}\"")), "{xml}");
    assert!(xml.contains(">black </w:t>"), "{xml}");
    assert!(xml.contains(">the</w:delText>"), "{xml}");
    let kinds: Vec<RevisionCategory> = document
        .revisions()
        .into_iter()
        .map(|r| r.category)
        .collect();
    assert_eq!(
        kinds,
        [
            RevisionCategory::Inline(RevisionKind::Insert),
            RevisionCategory::Inline(RevisionKind::Delete),
            RevisionCategory::Inline(RevisionKind::Insert),
        ]
    );
}

#[test]
fn whole_paragraph_changes_resolve_both_ways() {
    let cases: &[(&[&str], &[&str])] = &[
        (&["A", "B"], &["A", "B"]),
        (&["Alpha one", "Beta"], &["New first", "Alpha one", "Beta"]),
        (&["Alpha", "Beta"], &["Alpha", "Beta", "Gamma tail"]),
        (&["Alpha", "Beta"], &["Alpha", "Beta", "Gamma", "Delta"]),
        (&["Alpha", "Beta", "Gamma"], &["Alpha", "Beta"]),
        (&["Alpha", "Beta", "Gamma", "Delta"], &["Alpha", "Beta"]),
        (&["Alpha", "Beta", "Gamma"], &["Beta", "Gamma"]),
        (&["Alpha", "Beta", "Gamma"], &["Beta"]),
        (&["Alpha", "Beta", "Gamma"], &["Alpha", "Gamma"]),
        (&[""], &["First", "Second"]),
        (&["First", "Second"], &[""]),
        (&["One two three", "Keep"], &["Totally different", "Keep"]),
        (&["Keep", "Old words here"], &["Keep", "Brand new text"]),
        (
            &["Keep", "Old", "Older"],
            &["Keep", "New", "Newer", "Newest"],
        ),
        (
            &["The quick brown fox", "jumps", "over the dog"],
            &["The quick red fox", "leaps", "over the lazy dog", "again"],
        ),
        (&["Only"], &["Only", ""]),
        (&["Only", ""], &["Only"]),
        (&["x"], &["y"]),
    ];
    for (original, revised) in cases {
        assert_round_trips(original, revised);
    }
}

#[test]
fn a_paragraph_before_a_table_is_deleted_and_inserted_cleanly() {
    let table = "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";
    for (original, revised) in [
        (
            format!("{}{}{table}{}", p("Keep"), p("Gone"), p("End")),
            format!("{}{table}{}", p("Keep"), p("End")),
        ),
        (
            format!("{}{table}{}", p("Keep"), p("End")),
            format!("{}{}{table}{}", p("Keep"), p("Added"), p("End")),
        ),
    ] {
        let result = compare(&pkg(&original), &pkg(&revised));
        let document = &result.package.document;
        assert_eq!(texts(&resolved(document, true)), texts(&doc(&revised)));
        assert_eq!(texts(&resolved(document, false)), texts(&doc(&original)));
        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
    }
}

#[test]
fn deleted_text_keeps_original_formatting_and_inserted_text_the_revised() {
    let original = "<w:p><w:r><w:t xml:space=\"preserve\">Keep </w:t></w:r>\
        <w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r></w:p>";
    let revised = "<w:p><w:r><w:rPr><w:color w:val=\"FF0000\"/></w:rPr><w:t xml:space=\"preserve\">Keep </w:t></w:r>\
        <w:r><w:rPr><w:i/></w:rPr><w:t>italic</w:t></w:r></w:p>";
    let result = compare(&pkg(original), &pkg(revised));
    let xml = document_to_xml(&result.package.document);
    let del = &xml[xml.find("<w:del ").expect("a deletion")..];
    let del = &del[..del.find("</w:del>").unwrap()];
    assert!(
        del.contains("<w:b/>") && del.contains(">bold</w:delText>"),
        "{del}"
    );
    let ins = &xml[xml.find("<w:ins ").expect("an insertion")..];
    let ins = &ins[..ins.find("</w:ins>").unwrap()];
    assert!(
        ins.contains("<w:i/>") && ins.contains(">italic</w:t>"),
        "{ins}"
    );
    // The unchanged word carries the revised formatting.
    let accepted = document_to_xml(&resolved(&result.package.document, true));
    assert!(accepted.contains("w:val=\"FF0000\""), "{accepted}");
}

#[test]
fn inputs_are_compared_in_their_accepted_state() {
    let original = pkg("<w:p><w:r><w:t xml:space=\"preserve\">Hello </w:t></w:r>\
         <w:ins w:id=\"1\" w:author=\"Old\"><w:r><w:t>world</w:t></w:r></w:ins>\
         <w:del w:id=\"2\" w:author=\"Old\"><w:r><w:delText>gone</w:delText></w:r></w:del></w:p>");
    let revised = pkg(&p("Hello world"));
    let result = compare(&original, &revised);
    assert_eq!(result.package.document.revisions(), []);
    assert_eq!(texts(&result.package.document), ["Hello world"]);
    // The source package still holds its own revisions.
    assert_eq!(original.document.revisions().len(), 2);
}

#[test]
fn unsupported_input_revisions_are_reported_and_not_carried() {
    let original = pkg(
        "<w:p><w:moveFrom w:id=\"9\" w:author=\"Old\"><w:r><w:t>moved</w:t></w:r></w:moveFrom>\
         <w:r><w:t xml:space=\"preserve\"> text</w:t></w:r></w:p>",
    );
    let result = compare(&original, &pkg(&p("other text")));
    assert!(
        result.skipped.contains(&CompareSkip::UnsupportedRevision {
            revision: UnsupportedRevisionKind::MoveFrom
        }),
        "{:?}",
        result.skipped
    );
    let xml = document_to_xml(&result.package.document);
    assert!(!xml.contains("moveFrom"), "{xml}");
}

#[test]
fn same_shape_tables_are_compared_per_cell() {
    let table = |a: &str, b: &str| {
        format!(
            "<w:tbl><w:tr><w:tc>{}</w:tc><w:tc>{}</w:tc></w:tr></w:tbl>{}",
            p(a),
            p(b),
            p("after")
        )
    };
    let original = table("left cell", "right cell");
    let revised = table("left cell", "right changed cell");
    let result = compare(&pkg(&original), &pkg(&revised));
    let document = &result.package.document;
    assert!(result.skipped.is_empty(), "{:?}", result.skipped);
    assert!(!document.revisions().is_empty());
    assert_eq!(texts(&resolved(document, true)), texts(&doc(&revised)));
    assert_eq!(texts(&resolved(document, false)), texts(&doc(&original)));
    assert_eq!(
        texts(&resolved(&reloaded(&result), false)),
        texts(&doc(&original))
    );
}

#[test]
fn a_table_whose_shape_changed_is_kept_as_revised_and_reported() {
    let original = format!(
        "{}<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>{}",
        p("before"),
        p("one"),
        p("after")
    );
    let revised = format!(
        "{}<w:tbl><w:tr><w:tc>{}</w:tc><w:tc>{}</w:tc></w:tr></w:tbl>{}",
        p("before"),
        p("one"),
        p("two"),
        p("after")
    );
    let result = compare(&pkg(&original), &pkg(&revised));
    assert_eq!(result.skipped, [CompareSkip::Table { index: 1 }]);
    assert_eq!(
        texts(&result.package.document),
        ["before", "one", "two", "after"]
    );
}

#[test]
fn the_result_is_the_revised_package_with_unique_revision_ids() {
    let mut revised = new_markdown_package(doc(&paras(&["One two", "Three"])));
    // A revision id already used elsewhere in the revised package.
    revised.set_part_text(
        "word/styles.xml",
        &format!("<w:styles xmlns:w=\"{W_NS}\"><w:style w:styleId=\"Kept\"/><!-- w:id=\"40\" --></w:styles>"),
    );
    let original = pkg(&paras(&["One", "Zero", "Three four"]));
    let result = compare(&original, &revised);
    for name in revised.part_names() {
        if name != "word/document.xml" {
            assert_eq!(result.package.part(name), revised.part(name), "{name}");
        }
    }
    let ids: Vec<u64> = result
        .package
        .document
        .revisions()
        .iter()
        .map(|r| r.metadata.id.as_deref().unwrap().parse().unwrap())
        .collect();
    assert!(ids.len() >= 4, "{ids:?}");
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "unique ids {ids:?}");
    assert!(ids.iter().all(|&id| id > 40), "{ids:?}");
}

#[test]
fn deleted_paragraph_properties_are_made_safe_for_the_revised_package() {
    let original = pkg(concat!(
        "<w:p><w:pPr><w:pStyle w:val=\"OnlyInOriginal\"/>",
        "<w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"77\"/></w:numPr>",
        "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId42\"/></w:sectPr>",
        "</w:pPr><w:r><w:t>Removed section end</w:t></w:r></w:p>",
        "<w:p><w:r><w:t>Kept</w:t></w:r></w:p>"
    ));
    let result = compare(&original, &pkg(&p("Kept")));
    let xml = document_to_xml(&result.package.document);
    assert!(!xml.contains("rId42"), "{xml}");
    assert!(!xml.contains("OnlyInOriginal"), "{xml}");
    assert!(!xml.contains("w:numId"), "{xml}");
    assert_eq!(
        texts(&resolved(&result.package.document, false)),
        ["Removed section end", "Kept"]
    );
}

#[test]
fn hyperlinks_keep_the_revised_link_and_drop_the_original_one() {
    let original = pkg(
        "<w:p><w:hyperlink r:id=\"rId77\"><w:r><w:t>old link</w:t></w:r></w:hyperlink>\
         <w:r><w:t xml:space=\"preserve\"> and </w:t></w:r>\
         <w:hyperlink w:anchor=\"kept\"><w:r><w:t>kept link</w:t></w:r></w:hyperlink></w:p>",
    );
    let revised = pkg("<w:p><w:r><w:t xml:space=\"preserve\"> and </w:t></w:r>\
         <w:hyperlink w:anchor=\"kept\"><w:r><w:t>kept new link</w:t></w:r></w:hyperlink></w:p>");
    let result = compare(&original, &revised);
    let xml = document_to_xml(&result.package.document);
    assert!(!xml.contains("rId77"), "{xml}");
    assert!(xml.contains("w:anchor=\"kept\""), "{xml}");
    let document = &result.package.document;
    assert_eq!(texts(&resolved(document, true)), [" and kept new link"]);
    assert_eq!(
        texts(&resolved(document, false)),
        ["old link and kept link"]
    );
    let accepted = document_to_xml(&resolved(document, true));
    let link = &accepted[accepted.find("<w:hyperlink").unwrap()..];
    assert!(
        link[..link.find("</w:hyperlink>").unwrap()].contains("new"),
        "{accepted}"
    );
}

#[test]
fn zero_width_markers_are_kept_from_the_revised_side_only() {
    let original = pkg(
        "<w:p><w:commentRangeStart w:id=\"5\"/><w:r><w:t>Old text</w:t></w:r>\
         <w:commentRangeEnd w:id=\"5\"/><w:r><w:commentReference w:id=\"5\"/></w:r></w:p>",
    );
    let revised = pkg(
        "<w:p><w:r><w:t xml:space=\"preserve\">New </w:t></w:r><w:commentRangeStart w:id=\"0\"/>\
         <w:r><w:t>text</w:t></w:r><w:commentRangeEnd w:id=\"0\"/>\
         <w:r><w:commentReference w:id=\"0\"/></w:r><w:bookmarkStart w:id=\"3\" w:name=\"b\"/>\
         <w:bookmarkEnd w:id=\"3\"/></w:p>",
    );
    let result = compare(&original, &revised);
    for accept in [true, false] {
        let xml = document_to_xml(&resolved(&result.package.document, accept));
        assert!(
            !xml.contains("w:id=\"5\""),
            "original comment dropped: {xml}"
        );
        for marker in [
            "<w:commentRangeStart w:id=\"0\"/>",
            "<w:commentRangeEnd w:id=\"0\"/>",
            "<w:commentReference w:id=\"0\"/>",
            "<w:bookmarkStart w:id=\"3\"",
        ] {
            assert!(xml.contains(marker), "{marker} (accept={accept}): {xml}");
        }
    }
    assert_eq!(
        texts(&resolved(&result.package.document, false)),
        ["Old text"]
    );
}

#[test]
fn complex_fields_resolve_to_well_formed_field_structure() {
    let field = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>4</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
    // A field only in the revised document, then only in the original.
    for (original, revised) in [
        (
            p("Page here"),
            format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">Page </w:t></w:r>{field}<w:r><w:t xml:space=\"preserve\"> here</w:t></w:r></w:p>"
            ),
        ),
        (
            format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">Page </w:t></w:r>{field}<w:r><w:t xml:space=\"preserve\"> here</w:t></w:r></w:p>"
            ),
            p("Page here"),
        ),
    ] {
        let result = compare(&pkg(&original), &pkg(&revised));
        for accept in [true, false] {
            let resolved = resolved(&reloaded(&result), accept);
            let xml = document_to_xml(&resolved);
            let count = |needle: &str| xml.matches(needle).count();
            let begins = count("fldCharType=\"begin\"");
            assert_eq!(begins, count("fldCharType=\"end\""), "{xml}");
            assert!(count("fldCharType=\"separate\"") <= begins, "{xml}");
            assert!(
                !xml.contains("<w:delText") && !xml.contains("<w:ins "),
                "{xml}"
            );
            // Reloading the resolved document keeps it stable.
            let again = document_to_xml(&parse_document_xml(&xml, &Relationships::default()));
            assert_eq!(xml, again);
        }
    }
}

#[test]
fn the_same_picture_with_different_relationship_ids_is_unchanged() {
    let picture = |rid: &str| {
        format!(
            "<w:p><w:r><w:t xml:space=\"preserve\">Logo </w:t></w:r><w:r><w:drawing><wp:inline \
             xmlns:wp=\"urn:wp\"><a:blip xmlns:a=\"urn:a\" r:embed=\"{rid}\"/></wp:inline></w:drawing></w:r></w:p>"
        )
    };
    let result = compare(&pkg(&picture("rId5")), &pkg(&picture("rId9")));
    assert_eq!(result.package.document.revisions(), []);
    assert!(document_to_xml(&result.package.document).contains("rId9"));

    // A picture only in the original cannot be carried into the revised package.
    let result = compare(&pkg(&picture("rId5")), &pkg(&p("Logo ")));
    assert!(
        result.skipped.contains(&CompareSkip::Object),
        "{:?}",
        result.skipped
    );
    assert!(!document_to_xml(&result.package.document).contains("rId5"));
}

#[test]
fn a_deleted_note_reference_is_reported() {
    let original = pkg(
        "<w:p><w:r><w:t>Claim</w:t></w:r><w:r><w:rPr><w:rStyle w:val=\"FootnoteReference\"/></w:rPr>\
         <w:footnoteReference w:id=\"1\"/></w:r></w:p>",
    );
    let result = compare(&original, &pkg(&p("Claim")));
    assert!(
        result.skipped.contains(&CompareSkip::NoteRef),
        "{:?}",
        result.skipped
    );
    assert!(!document_to_xml(&result.package.document).contains("footnoteReference"));
}

#[test]
fn a_change_before_a_table_never_borrows_a_section_ending_mark() {
    let sect = "<w:p><w:pPr><w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr></w:pPr>\
        <w:r><w:t>P</w:t></w:r></w:p>";
    let table = "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";
    let non_empty =
        |d: &Document| -> Vec<String> { texts(d).into_iter().filter(|t| !t.is_empty()).collect() };
    let p_section = |d: &Document| -> bool {
        d.body.iter().any(|b| match b {
            Block::Paragraph(p) => p.plain_text() == "P" && p.props.section_break.is_some(),
            _ => false,
        })
    };
    // A deleted paragraph, then an inserted one, between P (which ends a
    // section) and a table.
    for (original, revised) in [
        (
            format!("{sect}{}{table}{}", p("Gone"), p("Q")),
            format!("{sect}{table}{}", p("Q")),
        ),
        (
            format!("{sect}{table}{}", p("Q")),
            format!("{sect}{}{table}{}", p("Added"), p("Q")),
        ),
    ] {
        let result = compare(&pkg(&original), &pkg(&revised));
        assert!(
            result
                .skipped
                .contains(&CompareSkip::ParagraphMark { index: 1 }),
            "{:?}",
            result.skipped
        );
        for document in [result.package.document.clone(), reloaded(&result)] {
            let accepted = resolved(&document, true);
            let rejected = resolved(&document, false);
            assert!(p_section(&accepted), "{}", document_to_xml(&accepted));
            assert!(p_section(&rejected), "{}", document_to_xml(&rejected));
            assert_eq!(non_empty(&accepted), non_empty(&doc(&revised)));
            assert_eq!(non_empty(&rejected), non_empty(&doc(&original)));
        }
    }
}

/// A package whose stored document part has `root` as its `<w:document>`
/// start tag, so a save re-emits those declarations.
fn pkg_with_root(root: &str, body: &str) -> Package {
    let xml = format!("{root}<w:body>{body}</w:body></w:document>");
    let mut package = new_package(parse_document_xml(&xml, &Relationships::default()));
    assert!(package.set_part_text("word/document.xml", &xml));
    package
}

fn saved_document_xml(result: &CompareResult) -> String {
    load_package(&save_package(&result.package))
        .expect("result reloads")
        .part_text("word/document.xml")
        .unwrap()
}

const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

#[test]
fn deleted_original_markup_keeps_its_namespace_prefixes_bound() {
    let original = pkg_with_root(
        &format!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:mc=\"{MC}\" xmlns:w14=\"{W14}\" mc:Ignorable=\"w14\">"
        ),
        "<w:p><w:r><w:t xml:space=\"preserve\">Keep </w:t></w:r>\
         <w:r><w:rPr><w14:ligatures w14:val=\"standard\"/></w:rPr><w:t>gone</w:t></w:r></w:p>",
    );
    let revised = pkg(&p("Keep "));
    let result = compare(&original, &revised);
    let xml = saved_document_xml(&result);
    let root = &xml[xml.find("<w:document").unwrap()..];
    let root = &root[..root.find('>').unwrap()];
    assert!(root.contains(&format!("xmlns:w14=\"{W14}\"")), "{root}");
    assert!(root.contains(&format!("xmlns:mc=\"{MC}\"")), "{root}");
    assert!(root.contains("mc:Ignorable=\"w14\""), "{root}");
    let del = &xml[xml.find("<w:del ").expect("a deletion")..];
    assert!(
        del[..del.find("</w:del>").unwrap()].contains("<w14:ligatures"),
        "{xml}"
    );
    assert!(result.skipped.is_empty(), "{:?}", result.skipped);
    assert_eq!(texts(&resolved(&reloaded(&result), false)), ["Keep gone"]);
}

#[test]
fn markup_whose_prefix_the_revised_root_binds_differently_is_dropped() {
    let original = pkg_with_root(
        &format!("<w:document xmlns:w=\"{W_NS}\" xmlns:w14=\"urn:not-w14\">"),
        "<w:p><w:r><w:t xml:space=\"preserve\">Keep </w:t></w:r>\
         <w:r><w:rPr><w14:odd/></w:rPr><w:t>gone</w:t></w:r></w:p>",
    );
    let revised = pkg_with_root(
        &format!("<w:document xmlns:w=\"{W_NS}\" xmlns:w14=\"{W14}\">"),
        &p("Keep "),
    );
    let result = compare(&original, &revised);
    let xml = saved_document_xml(&result);
    assert!(!xml.contains("w14:odd"), "{xml}");
    assert!(!xml.contains("urn:not-w14"), "{xml}");
    assert_eq!(result.skipped, [CompareSkip::Object]);
    assert_eq!(texts(&resolved(&reloaded(&result), false)), ["Keep gone"]);
}

#[test]
fn simple_fields_inside_revisions_become_run_level_complex_fields() {
    let field = |result: &str| {
        format!(
            "<w:fldSimple w:instr=\" DATE &amp; TIME \"><w:r><w:t>{result}</w:t></w:r></w:fldSimple>"
        )
    };
    let para =
        |field: &str| format!("<w:p><w:r><w:t xml:space=\"preserve\">On </w:t></w:r>{field}</w:p>");
    // Deleted, inserted, and a changed cached result.
    for (original, revised) in [
        (para(&field("1/2")), para("")),
        (para(""), para(&field("1/2"))),
        (para(&field("1/2")), para(&field("3/4"))),
    ] {
        let result = compare(&pkg(&original), &pkg(&revised));
        let xml = saved_document_xml(&result);
        for tag in ["<w:ins ", "<w:del "] {
            let mut rest = xml.as_str();
            while let Some(at) = rest.find(tag) {
                let close = if tag == "<w:ins " {
                    "</w:ins>"
                } else {
                    "</w:del>"
                };
                let wrapper = &rest[at..at + rest[at..].find(close).unwrap()];
                assert!(!wrapper.contains("<w:fldSimple"), "{wrapper}");
                rest = &rest[at + wrapper.len()..];
            }
        }
        assert!(xml.contains("<w:ins ") || xml.contains("<w:del "), "{xml}");
        let reloaded = reloaded(&result);
        let resolve = |accept| {
            resolved(&reloaded, accept)
                .plain_text()
                .trim_end()
                .to_string()
        };
        let plain = |body: &str| doc(body).plain_text().trim_end().to_string();
        assert_eq!(resolve(true), plain(&revised), "{xml}");
        assert_eq!(resolve(false), plain(&original), "{xml}");
    }
}
