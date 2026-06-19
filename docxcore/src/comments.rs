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
                let c = Comment {
                    id: p.attr("w:id").to_string(),
                    author: decode(p.attr("w:author")),
                    initials: decode(p.attr("w:initials")),
                    date: p.attr("w:date").to_string(),
                    text: collect_text(&mut p),
                    quoted: String::new(),
                };
                out.push(c);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

fn decode(raw: &str) -> String {
    let mut s = String::new();
    XmlParser::append_decoded(raw, &mut s);
    s
}

/// Consume the body of the just-started `w:comment`, returning its plain text
/// with paragraph breaks as newlines. Stops at the matching end tag.
fn collect_text(p: &mut XmlParser) -> String {
    let mut s = String::new();
    let mut depth = 1; // inside <w:comment>
    let mut in_t = false;
    loop {
        match p.next() {
            Event::Start => {
                match p.name() {
                    "w:t" => in_t = true,
                    "w:tab" => s.push('\t'),
                    "w:br" | "w:cr" => s.push('\n'),
                    _ => {}
                }
                depth += 1;
            }
            Event::Text => {
                if in_t {
                    XmlParser::append_decoded(p.text(), &mut s);
                }
            }
            Event::End => {
                match p.name() {
                    "w:t" => in_t = false,
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
    s.trim_matches('\n').to_string()
}

/// Scan `document.xml` once, returning (anchor-order by comment id, quoted span
/// by comment id). The quoted span is the run text covered by each comment's
/// `commentRangeStart`…`commentRangeEnd`.
fn anchors(doc: &str) -> (HashMap<String, usize>, HashMap<String, String>) {
    let mut order: HashMap<String, usize> = HashMap::new();
    let mut quotes: HashMap<String, String> = HashMap::new();
    let mut active: Vec<String> = Vec::new();
    let mut seq = 0usize;
    let mut in_t = false;
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
                "w:t" => in_t = true,
                _ => {}
            },
            Event::Text => {
                if in_t && !active.is_empty() {
                    let mut piece = String::new();
                    XmlParser::append_decoded(p.text(), &mut piece);
                    for id in &active {
                        if let Some(q) = quotes.get_mut(id) {
                            q.push_str(&piece);
                        }
                    }
                }
            }
            Event::End => {
                if p.name() == "w:t" {
                    in_t = false;
                }
            }
            Event::Eof => break,
        }
    }
    (order, quotes)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn no_comments_part_is_empty() {
        assert!(parse_comments_xml("<w:comments/>").is_empty());
    }
}
