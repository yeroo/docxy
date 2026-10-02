//! Word's Document Inspector (File > Info > Inspect Document): find and
//! remove comments, hidden text and personal document properties. Tracked
//! changes are resolved by accepting them ([`Document::accept_all_revisions`]),
//! so they need nothing here beyond a count.
//!
//! The body walk descends into everything that can hold runs or markers:
//! paragraphs, table cells, hyperlinks (both `runs` and `content`), tracked
//! change wrappers and text boxes. A hyperlink or revision wrapper that loaded
//! from the file saves its original `raw` XML until `content_changed` is set,
//! so removing anything inside one sets that flag; otherwise the save would
//! quietly write the removed content back.

use crate::model::{Block, Document, Inline, Run, RunProps};
use crate::package::Package;

/// What a body walk removes or counts.
#[derive(Clone, Copy)]
enum Target {
    /// Runs, tabs and breaks formatted `w:vanish`.
    Hidden,
    /// `w:commentRangeStart`, `w:commentRangeEnd` and `w:commentReference`.
    CommentMarkers,
}

impl Target {
    fn matches(self, inline: &Inline) -> bool {
        match (self, inline) {
            (Target::Hidden, Inline::Run(r)) => is_hidden(&r.props),
            (Target::Hidden, Inline::Tab(p) | Inline::Break(_, p)) => is_hidden(p),
            (Target::CommentMarkers, Inline::Raw(raw)) => is_comment_marker(raw),
            _ => false,
        }
    }

    fn matches_run(self, run: &Run) -> bool {
        matches!(self, Target::Hidden) && is_hidden(&run.props)
    }
}

/// Whether direct run formatting hides the text the way Word's inspector
/// means it: `w:vanish`. The loader maps `w:webHidden` (hidden only in Web
/// layout; every TOC puts it on its page numbers) to `vanish` too and keeps
/// the element in `raw_props`, so a run carrying `webHidden` is not hidden
/// text. A run with both `vanish` and `webHidden` is therefore missed: the
/// model cannot tell it from `webHidden` alone.
fn is_hidden(props: &RunProps) -> bool {
    props.vanish
        && !props
            .raw_props
            .iter()
            .any(|r| raw_local_name(r) == "webHidden")
}

/// The local name of a preserved element's XML (`<w:webHidden/>` -> `webHidden`).
fn raw_local_name(raw: &str) -> &str {
    let tag = raw.trim_start().trim_start_matches('<');
    let end = tag
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(tag.len());
    let name = &tag[..end];
    name.rsplit(':').next().unwrap_or(name)
}

fn is_comment_marker(raw: &str) -> bool {
    [
        "w:commentRangeStart",
        "w:commentRangeEnd",
        "w:commentReference",
    ]
    .iter()
    .any(|tag| raw.contains(tag))
}

fn count_blocks(blocks: &[Block], target: Target) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => count_inlines(&p.content, target),
            Block::Table(t) => t
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .map(|cell| count_blocks(&cell.blocks, target))
                .sum(),
            Block::SectionProperties(_) | Block::Raw(_) => 0,
        })
        .sum()
}

fn count_inlines(content: &[Inline], target: Target) -> usize {
    content
        .iter()
        .map(|inline| {
            let own = usize::from(target.matches(inline));
            own + match inline {
                Inline::Hyperlink(h) => {
                    h.runs.iter().filter(|r| target.matches_run(r)).count()
                        + count_inlines(&h.content, target)
                }
                Inline::Revision { content, .. } => count_inlines(content, target),
                Inline::TextBox { blocks, .. } => count_blocks(blocks, target),
                _ => 0,
            }
        })
        .sum()
}

fn remove_blocks(blocks: &mut [Block], target: Target) -> usize {
    let mut removed = 0;
    for b in blocks {
        match b {
            Block::Paragraph(p) => removed += remove_inlines(&mut p.content, target),
            Block::Table(t) => {
                for cell in t.rows.iter_mut().flat_map(|row| &mut row.cells) {
                    removed += remove_blocks(&mut cell.blocks, target);
                }
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
    }
    removed
}

fn remove_inlines(content: &mut Vec<Inline>, target: Target) -> usize {
    let before = content.len();
    content.retain(|inline| !target.matches(inline));
    let mut removed = before - content.len();
    for inline in content.iter_mut() {
        match inline {
            Inline::Hyperlink(h) => {
                let runs_before = h.runs.len();
                h.runs.retain(|r| !target.matches_run(r));
                let inside = runs_before - h.runs.len() + remove_inlines(&mut h.content, target);
                if inside > 0 {
                    h.content_changed = true;
                }
                removed += inside;
            }
            Inline::Revision {
                content,
                content_changed,
                ..
            } => {
                let inside = remove_inlines(content, target);
                if inside > 0 {
                    *content_changed = true;
                }
                removed += inside;
            }
            // A text box's `txbxContent` is always rewritten from `blocks`.
            Inline::TextBox { blocks, .. } => removed += remove_blocks(blocks, target),
            _ => {}
        }
    }
    removed
}

/// Hidden runs, tabs and breaks in the body (see [`is_hidden`]).
pub fn count_hidden_runs(doc: &Document) -> usize {
    count_blocks(&doc.body, Target::Hidden)
}

/// Remove every hidden run, tab and break from the body. Returns how many.
pub fn remove_hidden_text(doc: &mut Document) -> usize {
    remove_blocks(&mut doc.body, Target::Hidden)
}

/// Whether any comment marker (range start/end or reference) is in the body.
pub fn has_comment_markers(doc: &Document) -> bool {
    count_blocks(&doc.body, Target::CommentMarkers) > 0
}

/// Remove every comment marker of every comment from the body. Returns how many.
pub fn remove_all_comment_markers(doc: &mut Document) -> usize {
    remove_blocks(&mut doc.body, Target::CommentMarkers)
}

const COMMENTS_PART: &str = "word/comments.xml";

/// Rewrite `word/comments.xml` to its root element alone: the opening
/// `<w:comments …>` tag (namespaces kept) and its close, no comments. Done
/// directly rather than per id, since removing one comment matches its
/// opening tag literally and misses a producer's other attribute order.
/// Returns false when there is no such part or it already has no children.
pub fn empty_comments_part(pkg: &mut Package) -> bool {
    let Some(xml) = pkg.part_text(COMMENTS_PART) else {
        return false;
    };
    match emptied_root(&xml) {
        Some(emptied) if emptied != xml => pkg.set_part_text(COMMENTS_PART, &emptied),
        _ => false,
    }
}

/// `xml` with everything inside its root element dropped, or `None` when it
/// has no root element. A self-closing root is returned unchanged.
fn emptied_root(xml: &str) -> Option<String> {
    let mut from = 0;
    let start = loop {
        let at = from + xml[from..].find('<')?;
        if xml[at + 1..].starts_with(['?', '!']) {
            from = at + 1;
        } else {
            break at;
        }
    };
    let end = start + tag_end(&xml[start..])?;
    let opening = &xml[..end];
    if opening.ends_with("/>") {
        return Some(xml.to_string());
    }
    let name = &xml[start + 1..]
        [..xml[start + 1..].find(|c: char| c.is_whitespace() || c == '>' || c == '/')?];
    Some(format!("{opening}</{name}>"))
}

/// The byte length of the tag starting at `s[0] == '<'`, through its `>`,
/// skipping `>` inside quoted attribute values.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(i + 1),
            _ => {}
        }
    }
    None
}

const CORE_PART: &str = "docProps/core.xml";
const APP_PART: &str = "docProps/app.xml";
const CUSTOM_PART: &str = "docProps/custom.xml";

/// The personal properties Word's inspector removes from `core.xml`. The
/// creation/modification dates and `cp:revision` stay. The prefixes are
/// matched literally: Word and the other producers we know write exactly
/// these.
const CORE_PERSONAL: [&str; 8] = [
    "dc:creator",
    "cp:lastModifiedBy",
    "dc:title",
    "dc:subject",
    "cp:keywords",
    "dc:description",
    "cp:category",
    "cp:contentStatus",
];
const APP_PERSONAL: [&str; 2] = ["Company", "Manager"];
const CUSTOM_PERSONAL: [&str; 1] = ["property"];

/// One element named `name`: its byte range and whether it has non-blank content.
fn find_element(xml: &str, name: &str, from: usize) -> Option<(usize, usize, bool)> {
    let open = format!("<{name}");
    let mut at = from;
    loop {
        let start = at + xml[at..].find(&open)?;
        let after = start + open.len();
        // `<dc:title` must not match `<dc:titleX`.
        if !xml[after..].starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/') {
            at = after;
            continue;
        }
        let head_end = start + tag_end(&xml[start..])?;
        if xml[..head_end].ends_with("/>") {
            return Some((start, head_end, false));
        }
        let close = format!("</{name}>");
        let close_at = head_end + xml[head_end..].find(&close)?;
        let filled = !xml[head_end..close_at].trim().is_empty();
        return Some((start, close_at + close.len(), filled));
    }
}

fn has_filled(xml: &str, names: &[&str]) -> bool {
    names.iter().any(|name| {
        let mut from = 0;
        while let Some((_, end, filled)) = find_element(xml, name, from) {
            if filled {
                return true;
            }
            from = end;
        }
        false
    })
}

fn remove_all(xml: &str, names: &[&str]) -> String {
    let mut out = xml.to_string();
    for name in names {
        while let Some((start, end, _)) = find_element(&out, name, 0) {
            out.replace_range(start..end, "");
        }
    }
    out
}

fn property_parts() -> [(&'static str, &'static [&'static str]); 3] {
    [
        (CORE_PART, &CORE_PERSONAL),
        (APP_PART, &APP_PERSONAL),
        (CUSTOM_PART, &CUSTOM_PERSONAL),
    ]
}

/// Whether the package holds personal document properties: a non-empty
/// author, title, … in `core.xml`, a company or manager in `app.xml`, or any
/// custom property. Empty elements (`<dc:title/>`) don't count.
pub fn has_personal_properties(pkg: &Package) -> bool {
    property_parts().iter().any(|(part, names)| {
        pkg.part_text(part)
            .is_some_and(|xml| has_filled(&xml, names))
    })
}

/// Remove the personal document properties (see [`has_personal_properties`]),
/// empty ones included. Returns whether any part changed.
pub fn remove_personal_properties(pkg: &mut Package) -> bool {
    let mut changed = false;
    for (part, names) in property_parts() {
        let Some(xml) = pkg.part_text(part) else {
            continue;
        };
        let out = remove_all(&xml, names);
        if out != xml {
            changed |= pkg.set_part_text(part, &out);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::{Relationships, parse_document_xml};
    use crate::package::{load_package, save_package};
    use crate::serialize::document_to_xml;
    use crate::zipwrite::write_zip;

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
                     xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";

    fn parse(body: &str) -> Document {
        let xml = format!("<w:document {W}><w:body>{body}</w:body></w:document>");
        parse_document_xml(&xml, &Relationships::default())
    }

    fn docx(document_xml: &str, extra: &[(&str, &str)]) -> Package {
        let mut parts = vec![
            (
                "[Content_Types].xml".to_string(),
                br#"<?xml version="1.0"?><Types/>"#.to_vec(),
            ),
            (
                "_rels/.rels".to_string(),
                br#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#.to_vec(),
            ),
            ("word/document.xml".to_string(), document_xml.as_bytes().to_vec()),
            (
                "word/styles.xml".to_string(),
                br#"<?xml version="1.0"?><w:styles/>"#.to_vec(),
            ),
        ];
        for (name, xml) in extra {
            parts.push((name.to_string(), xml.as_bytes().to_vec()));
        }
        load_package(&write_zip(&parts)).unwrap()
    }

    const VISIBLE: &str = "<w:r><w:t>keep</w:t></w:r>";
    const HIDDEN: &str = "<w:r><w:rPr><w:vanish/></w:rPr><w:t>gone</w:t></w:r>";

    #[test]
    fn inspect_counts_and_removes_hidden_runs() {
        let body = format!(
            "<w:p>{VISIBLE}{HIDDEN}</w:p>\
             <w:tbl><w:tr><w:tc><w:p>{HIDDEN}{VISIBLE}</w:p></w:tc></w:tr></w:tbl>\
             <w:p><w:hyperlink w:anchor=\"a\">{VISIBLE}{HIDDEN}</w:hyperlink></w:p>\
             <w:p><w:hyperlink w:anchor=\"b\">{VISIBLE}<w:ins w:id=\"7\" w:author=\"A\">{HIDDEN}</w:ins></w:hyperlink></w:p>\
             <w:p><w:ins w:id=\"8\" w:author=\"A\">{VISIBLE}{HIDDEN}</w:ins></w:p>"
        );
        let mut doc = parse(&body);
        assert_eq!(count_hidden_runs(&doc), 5);
        assert_eq!(remove_hidden_text(&mut doc), 5);
        assert_eq!(count_hidden_runs(&doc), 0);
        let texts: Vec<String> = doc.body.iter().map(|b| b.plain_text()).collect();
        assert!(texts.iter().all(|t| !t.contains("gone")), "{texts:?}");
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("gone"), "a raw-backed wrapper kept it: {xml}");
        assert!(!xml.contains("w:vanish"), "{xml}");
        assert_eq!(xml.matches("keep").count(), 5, "{xml}");
        // Each touched wrapper is rebuilt rather than written from `raw`.
        for b in &doc.body {
            let Block::Paragraph(p) = b else { continue };
            for inline in &p.content {
                match inline {
                    Inline::Hyperlink(h) => assert!(h.content_changed),
                    Inline::Revision {
                        content_changed, ..
                    } => assert!(content_changed),
                    _ => {}
                }
            }
        }
        assert_eq!(remove_hidden_text(&mut doc), 0);
    }

    #[test]
    fn inspect_web_hidden_is_not_hidden_text() {
        // A Word TOC entry: the tab and PAGEREF page number are webHidden.
        let body = "<w:p><w:hyperlink w:anchor=\"_Toc1\">\
            <w:r><w:t>Intro</w:t></w:r>\
            <w:r><w:rPr><w:webHidden/></w:rPr><w:tab/></w:r>\
            <w:r><w:rPr><w:webHidden/></w:rPr><w:t>3</w:t></w:r>\
            </w:hyperlink></w:p>";
        let mut doc = parse(body);
        let loaded = doc.clone();
        assert_eq!(count_hidden_runs(&doc), 0);
        assert_eq!(remove_hidden_text(&mut doc), 0);
        assert_eq!(doc, loaded);
    }

    #[test]
    fn inspect_removes_all_comment_markers_everywhere() {
        let marked = |id: u32| {
            format!(
                "<w:commentRangeStart w:id=\"{id}\"/>{VISIBLE}<w:commentRangeEnd w:id=\"{id}\"/>\
                 <w:r><w:commentReference w:id=\"{id}\"/></w:r>"
            )
        };
        let body = format!(
            "<w:p>{}<w:bookmarkStart w:id=\"0\" w:name=\"bm\"/>{VISIBLE}<w:bookmarkEnd w:id=\"0\"/></w:p>\
             <w:tbl><w:tr><w:tc><w:p>{}</w:p></w:tc></w:tr></w:tbl>\
             <w:p><w:ins w:id=\"9\" w:author=\"A\">{}</w:ins></w:p>\
             <w:p><w:hyperlink w:anchor=\"a\"><w:ins w:id=\"10\" w:author=\"A\">{VISIBLE}</w:ins>{}</w:hyperlink></w:p>",
            marked(1),
            marked(2),
            marked(1),
            marked(2)
        );
        let mut doc = parse(&body);
        assert!(has_comment_markers(&doc));
        assert!(remove_all_comment_markers(&mut doc) >= 8);
        assert!(!has_comment_markers(&doc));
        let xml = document_to_xml(&doc);
        assert!(!xml.contains("comment"), "{xml}");
        assert!(xml.contains("w:bookmarkStart"), "other raw kept: {xml}");
        assert_eq!(xml.matches("keep").count(), 6, "{xml}");
    }

    #[test]
    fn inspect_empties_comments_part_author_before_id() {
        let comments = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
            <w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:w14=\"x\">\
            <w:comment w:author=\"Ann\" w:id=\"0\" w:date=\"2026-01-01T00:00:00Z\"><w:p><w:r><w:t>a &gt; b</w:t></w:r></w:p></w:comment>\
            <w:comment w:initials=\"B\" w:id=\"1\"><w:p/></w:comment></w:comments>";
        let mut pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[("word/comments.xml", comments)],
        );
        assert!(empty_comments_part(&mut pkg));
        let xml = pkg.part_text("word/comments.xml").unwrap();
        assert_eq!(
            xml,
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
             <w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:w14=\"x\"></w:comments>"
        );
        assert!(!empty_comments_part(&mut pkg), "already empty");
        let saved = load_package(&save_package(&pkg)).unwrap();
        assert!(
            !saved
                .part_text("word/comments.xml")
                .unwrap()
                .contains("<w:comment ")
        );
    }

    #[test]
    fn inspect_removes_personal_core_and_app_properties() {
        let core = "<?xml version=\"1.0\"?><cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\" xmlns:dcterms=\"t\">\
            <dc:title xml:lang=\"en\">Plan</dc:title><dc:creator>Ann</dc:creator>\
            <cp:lastModifiedBy>Bob</cp:lastModifiedBy><cp:keywords/>\
            <cp:revision>4</cp:revision>\
            <dcterms:created xsi:type=\"dcterms:W3CDTF\">2026-01-01T00:00:00Z</dcterms:created>\
            </cp:coreProperties>";
        let app = "<Properties xmlns=\"p\"><Application>Microsoft Office Word</Application>\
            <Company>Acme</Company><Manager/></Properties>";
        let mut pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[("docProps/core.xml", core), ("docProps/app.xml", app)],
        );
        assert!(has_personal_properties(&pkg));
        assert!(remove_personal_properties(&mut pkg));
        assert!(!has_personal_properties(&pkg));
        let core = pkg.part_text("docProps/core.xml").unwrap();
        for gone in ["dc:title", "dc:creator", "cp:lastModifiedBy", "cp:keywords"] {
            assert!(!core.contains(gone), "{gone}: {core}");
        }
        assert!(core.contains("<cp:revision>4</cp:revision>"), "{core}");
        assert!(core.contains("<dcterms:created"), "{core}");
        let app = pkg.part_text("docProps/app.xml").unwrap();
        assert_eq!(
            app,
            "<Properties xmlns=\"p\"><Application>Microsoft Office Word</Application></Properties>"
        );
        assert!(!remove_personal_properties(&mut pkg), "nothing left");
    }

    #[test]
    fn inspect_empty_properties_not_found() {
        let core = "<cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\"><dc:title/>\
            <dc:creator></dc:creator><dc:subject>  </dc:subject><dc:titleX>t</dc:titleX></cp:coreProperties>";
        let pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[("docProps/core.xml", core)],
        );
        assert!(!has_personal_properties(&pkg));
    }

    #[test]
    fn inspect_custom_properties_emptied() {
        let custom = "<Properties xmlns=\"c\" xmlns:vt=\"v\">\
            <property fmtid=\"{D5CDD505-2E9C-101B-9397-08002B2CF9AE}\" pid=\"2\" name=\"Client\"><vt:lpwstr>Acme</vt:lpwstr></property>\
            <property fmtid=\"{D5CDD505-2E9C-101B-9397-08002B2CF9AE}\" pid=\"3\" name=\"Ref\"><vt:i4>7</vt:i4></property>\
            </Properties>";
        let mut pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[("docProps/custom.xml", custom)],
        );
        assert!(has_personal_properties(&pkg));
        assert!(remove_personal_properties(&mut pkg));
        assert_eq!(
            pkg.part_text("docProps/custom.xml").unwrap(),
            "<Properties xmlns=\"c\" xmlns:vt=\"v\"></Properties>"
        );
        assert!(!has_personal_properties(&pkg));
    }

    #[test]
    fn no_property_parts_is_not_found() {
        let mut pkg = docx(
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
            &[],
        );
        assert!(!has_personal_properties(&pkg));
        assert!(!remove_personal_properties(&mut pkg));
        assert!(!empty_comments_part(&mut pkg));
    }
}
