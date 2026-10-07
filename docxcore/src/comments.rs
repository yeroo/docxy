//! Parse `word/comments.xml` and the comment anchor ranges in
//! `word/document.xml` into a flat list of [`Comment`]s for display.
//!
//! This is a read-only, display-oriented view: it extracts each comment's
//! author/initials/date and body text, plus the document text the comment is
//! anchored to (the run text between `w:commentRangeStart` and the matching
//! `w:commentRangeEnd`). Comments are returned in the order their anchors appear
//! in the document; any comment with no anchor is appended in file order.

use crate::package::Package;
use crate::xml::{Event, XmlParser};
use std::collections::HashMap;

/// One review comment, flattened for display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub initials: String,
    pub date: String,
    /// The comment body as plain text (paragraphs joined by newlines).
    pub text: String,
    /// The document text the comment is anchored to (may be empty).
    pub quoted: String,
    /// Whether the comment is resolved: `w15:done` on its `commentEx` in
    /// `word/commentsExtended.xml` (keyed by `para_id`).
    pub resolved: bool,
    /// The `w14:paraId` of the comment's last paragraph, the key
    /// `commentsExtended.xml` and `commentsIds.xml` use for it.
    pub para_id: Option<String>,
}

/// The part holding each comment's `w15:done` (resolved) state.
pub const COMMENTS_EXTENDED_PART: &str = "word/commentsExtended.xml";

/// A reviewer's initials as Word derives them from the user name: the first
/// letter of each word, uppercased (`Jane doe` is `JD`).
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .flat_map(char::to_uppercase)
        .collect()
}

/// Parse every comment in `pkg`, ordered by where each is anchored in the body.
pub fn parse_comments(pkg: &Package) -> Vec<Comment> {
    let xml = match pkg.part("word/comments.xml") {
        Some(b) => std::str::from_utf8(b).unwrap_or(""),
        None => return Vec::new(),
    };
    let mut comments = parse_comments_xml(xml);
    if comments.is_empty() {
        return comments;
    }
    if let Some(ext) = pkg.part_text(COMMENTS_EXTENDED_PART) {
        let done = parse_comments_extended(&ext);
        for c in &mut comments {
            c.resolved = c
                .para_id
                .as_ref()
                .and_then(|id| done.get(id))
                .copied()
                .unwrap_or(false);
        }
    }
    if let Some(doc) = pkg
        .part("word/document.xml")
        .and_then(|b| std::str::from_utf8(b).ok())
    {
        let (order, quotes) = anchors(doc);
        for c in &mut comments {
            if let Some(q) = quotes.get(&c.id) {
                c.quoted = q.clone();
            }
        }
        comments.sort_by_key(|c| order.get(&c.id).copied().unwrap_or(usize::MAX));
    }
    comments
}

/// Parse the `<w:comment>` entries of a `comments.xml` document.
pub fn parse_comments_xml(xml: &str) -> Vec<Comment> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if p.name() == "w:comment" => {
                let mut c = Comment {
                    id: p.attr("w:id").to_string(),
                    author: decode(p.attr("w:author")),
                    initials: decode(p.attr("w:initials")),
                    date: p.attr("w:date").to_string(),
                    ..Comment::default()
                };
                (c.text, c.para_id) = collect_text(&mut p);
                out.push(c);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Each `w15:commentEx`'s `w15:paraId` with whether it is `w15:done`.
pub fn parse_comments_extended(xml: &str) -> HashMap<String, bool> {
    crate::load::start_tags(xml, "w15:commentEx")
        .into_iter()
        .filter_map(|(_, tag)| {
            let id = crate::load::xml_attr_value(tag, "w15:paraId")?;
            let done = crate::load::xml_attr_value(tag, "w15:done")
                .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "on"));
            Some((id, done))
        })
        .collect()
}

fn decode(raw: &str) -> String {
    let mut s = String::new();
    XmlParser::append_decoded(raw, &mut s);
    s
}

/// Consume the body of the just-started `w:comment`, returning its plain text
/// with paragraph breaks as newlines. Stops at the matching end tag.
fn collect_text(p: &mut XmlParser) -> (String, Option<String>) {
    let mut s = String::new();
    let mut para_id = None;
    let mut depth = 1; // inside <w:comment>
    loop {
        match p.next() {
            Event::Start => {
                match p.name() {
                    "w:t" | "w:delText" => {
                        // Consumes through the element's End.
                        s.push_str(&crate::load::read_text(p));
                        continue;
                    }
                    "w:tab" => s.push('\t'),
                    "w:br" | "w:cr" => s.push('\n'),
                    "w:p" => {
                        // The last paragraph's id is the comment's.
                        para_id =
                            Some(p.attr("w14:paraId").to_string()).filter(|id| !id.is_empty());
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::Text => {}
            Event::End => {
                match p.name() {
                    // a paragraph closing inside the comment → a line break
                    "w:p" if depth >= 2 => s.push('\n'),
                    _ => {}
                }
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
        }
    }
    (s.trim_matches('\n').to_string(), para_id)
}

/// Scan `document.xml` once, returning (anchor-order by comment id, quoted span
/// by comment id). The quoted span is the run text covered by each comment's
/// `commentRangeStart`…`commentRangeEnd`.
fn anchors(doc: &str) -> (HashMap<String, usize>, HashMap<String, String>) {
    let mut order: HashMap<String, usize> = HashMap::new();
    let mut quotes: HashMap<String, String> = HashMap::new();
    let mut active: Vec<String> = Vec::new();
    let mut seq = 0usize;
    let mut p = XmlParser::new(doc);
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:commentRangeStart" => {
                    let id = p.attr("w:id").to_string();
                    order.entry(id.clone()).or_insert_with(|| {
                        let s = seq;
                        seq += 1;
                        s
                    });
                    quotes.entry(id.clone()).or_default();
                    active.push(id);
                }
                "w:commentRangeEnd" => {
                    let id = p.attr("w:id");
                    if let Some(pos) = active.iter().rposition(|x| x == id) {
                        active.remove(pos);
                    }
                }
                "w:commentReference" => {
                    let id = p.attr("w:id").to_string();
                    order.entry(id).or_insert_with(|| {
                        let s = seq;
                        seq += 1;
                        s
                    });
                }
                "w:t" => {
                    let piece = crate::load::read_text(&mut p);
                    for id in &active {
                        if let Some(q) = quotes.get_mut(id) {
                            q.push_str(&piece);
                        }
                    }
                }
                _ => {}
            },
            Event::Text | Event::End => {}
            Event::Eof => break,
        }
    }
    (order, quotes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_of_name() {
        assert_eq!(initials("Jane doe"), "JD");
        assert_eq!(initials("  ada   lovelace byron "), "ALB");
        assert_eq!(initials("boris"), "B");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn parses_author_date_and_text() {
        let xml = r#"<w:comments xmlns:w="x">
            <w:comment w:id="1" w:author="Jane Doe" w:initials="JD" w:date="2020-01-02T03:04:00Z">
              <w:p><w:r><w:t>Please clarify</w:t></w:r></w:p>
            </w:comment>
        </w:comments>"#;
        let cs = parse_comments_xml(xml);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].id, "1");
        assert_eq!(cs[0].author, "Jane Doe");
        assert_eq!(cs[0].initials, "JD");
        assert_eq!(cs[0].text, "Please clarify");
    }

    #[test]
    fn multi_paragraph_comment_joins_with_newlines() {
        let xml = r#"<w:comments xmlns:w="x">
            <w:comment w:id="7" w:author="A">
              <w:p><w:r><w:t>line one</w:t></w:r></w:p>
              <w:p><w:r><w:t>line two</w:t></w:r></w:p>
            </w:comment>
        </w:comments>"#;
        let cs = parse_comments_xml(xml);
        assert_eq!(cs[0].text, "line one\nline two");
    }

    #[test]
    fn decodes_entities_in_author_and_text() {
        let xml = r#"<w:comments xmlns:w="x">
            <w:comment w:id="1" w:author="A &amp; B">
              <w:p><w:r><w:t>x &lt; y</w:t></w:r></w:p>
            </w:comment>
        </w:comments>"#;
        let cs = parse_comments_xml(xml);
        assert_eq!(cs[0].author, "A & B");
        assert_eq!(cs[0].text, "x < y");
    }

    #[test]
    fn anchors_capture_quoted_span_and_order() {
        let doc = r#"<w:document xmlns:w="x"><w:body>
          <w:p>
            <w:commentRangeStart w:id="2"/><w:r><w:t>second</w:t></w:r><w:commentRangeEnd w:id="2"/>
            <w:r><w:commentReference w:id="2"/></w:r>
          </w:p>
          <w:p>
            <w:commentRangeStart w:id="1"/><w:r><w:t>first</w:t></w:r><w:commentRangeEnd w:id="1"/>
            <w:r><w:commentReference w:id="1"/></w:r>
          </w:p>
        </w:body></w:document>"#;
        let (order, quotes) = anchors(doc);
        assert_eq!(quotes.get("2").map(String::as_str), Some("second"));
        assert_eq!(quotes.get("1").map(String::as_str), Some("first"));
        // id 2 is anchored before id 1
        assert!(order["2"] < order["1"]);
    }

    /// Comment text and the quoted span use Word's reading of `w:t`: edge
    /// whitespace counts only under `xml:space="preserve"` (#1084).
    #[test]
    fn comment_and_quote_text_drop_unpreserved_edge_whitespace() {
        let xml = r#"<w:comments xmlns:w="x"><w:comment w:id="1" w:author="A">
            <w:p><w:r><w:t>SDT </w:t></w:r><w:r><w:t> Run</w:t></w:r>
            <w:r><w:t xml:space="preserve"> kept</w:t></w:r></w:p>
        </w:comment></w:comments>"#;
        assert_eq!(parse_comments_xml(xml)[0].text, "SDTRun kept");
        let doc = r#"<w:document xmlns:w="x"><w:body><w:p xml:space="preserve">
            <w:commentRangeStart w:id="1"/><w:r><w:t>SDT </w:t></w:r>
            <w:r><w:t xml:space="default"> Run</w:t></w:r><w:commentRangeEnd w:id="1"/>
        </w:p></w:body></w:document>"#;
        let (_, quotes) = anchors(doc);
        assert_eq!(quotes.get("1").map(String::as_str), Some("SDT Run"));
    }

    /// Deleted text in a comment reads like note text: `w:delText` under the
    /// same whitespace rule.
    #[test]
    fn comment_deleted_text_follows_the_whitespace_rule() {
        let comment = |attrs: &str| {
            format!(
                r#"<w:comments xmlns:w="x"><w:comment w:id="1" w:author="A"><w:p>
                <w:del w:id="2" w:author="A"><w:r><w:delText{attrs}> gone </w:delText></w:r></w:del>
                </w:p></w:comment></w:comments>"#
            )
        };
        assert_eq!(parse_comments_xml(&comment(""))[0].text, "gone");
        assert_eq!(
            parse_comments_xml(&comment(r#" xml:space="preserve""#))[0].text,
            " gone "
        );
    }

    #[test]
    fn no_comments_part_is_empty() {
        assert!(parse_comments_xml("<w:comments/>").is_empty());
    }

    /// Full [`parse_comments`] pipeline (`word/comments.xml` +
    /// `word/document.xml` anchors) against a real [`Package`], the shape
    /// `docxy`'s `doc.comments` control verb marshals directly into JSON.
    #[test]
    fn parse_comments_from_package_orders_by_anchor_and_fills_quoted_text() {
        use crate::package::load_package;
        use crate::zipwrite::write_zip;

        let comments_xml = r#"<?xml version="1.0"?><w:comments xmlns:w="x">
            <w:comment w:id="2" w:author="Bob" w:initials="B" w:date="2020-02-02T00:00:00Z">
              <w:p><w:r><w:t>second thought</w:t></w:r></w:p>
            </w:comment>
            <w:comment w:id="1" w:author="Ann" w:initials="A" w:date="2020-01-01T00:00:00Z">
              <w:p><w:r><w:t>first thought</w:t></w:r></w:p>
            </w:comment>
        </w:comments>"#;
        let document_xml = r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
            <w:p>
              <w:commentRangeStart w:id="1"/><w:r><w:t>alpha</w:t></w:r><w:commentRangeEnd w:id="1"/>
              <w:r><w:commentReference w:id="1"/></w:r>
            </w:p>
            <w:p>
              <w:commentRangeStart w:id="2"/><w:r><w:t>beta</w:t></w:r><w:commentRangeEnd w:id="2"/>
              <w:r><w:commentReference w:id="2"/></w:r>
            </w:p>
        </w:body></w:document>"#;
        let ct = r#"<?xml version="1.0"?><Types/>"#;
        let rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
        let bytes = write_zip(&[
            ("[Content_Types].xml".into(), ct.into()),
            ("_rels/.rels".into(), rels.into()),
            ("word/document.xml".into(), document_xml.into()),
            ("word/styles.xml".into(), "<w:styles/>".into()),
            ("word/comments.xml".into(), comments_xml.into()),
        ]);
        let pkg = load_package(&bytes).expect("load");
        let cs = parse_comments(&pkg);
        assert_eq!(cs.len(), 2);
        // Anchored in the body as id 1, then id 2 — despite comments.xml
        // listing id 2 first.
        assert_eq!(cs[0].id, "1");
        assert_eq!(cs[0].author, "Ann");
        assert_eq!(cs[0].text, "first thought");
        assert_eq!(cs[0].quoted, "alpha");
        assert_eq!(cs[1].id, "2");
        assert_eq!(cs[1].quoted, "beta");
    }
}
