//! A document clip from another app's HTML copy (#1950): what a document
//! paste takes when the clipboard holds HTML but no RTF, as a browser's copy
//! does (Firefox and Chrome offer `text/html`, never RTF).
//!
//! The HTML importer ([`crate::import::html`]) reads the page, and the clip
//! is mapped from its document as the RTF's is ([`crate::clip_rtf`]): its
//! paragraphs, a table row as tab-joined cells, the trailing mark from the
//! plain text copied beside it. Pictures are not read (a clip holds no
//! media), and paragraph formatting, lists included, is not in a clip.

use crate::editor::Clip;
use crate::import::Budget;
use crate::import::html::import_html_str_within;

impl Clip {
    /// The clip another app's HTML copy holds, or `None` when `html` holds
    /// no text or is past what an import may cost: the paste then takes the
    /// plain text. `plain` is the text copied with it: it says whether the
    /// clip ends with a paragraph mark, as in [`Clip::from_rtf`]. The
    /// clipboard's HTML is text already (UTF-8, a UTF-16 page converted
    /// where it is read), so no charset is read from it: a `charset=` an
    /// Office page or its words carry says nothing about these characters.
    pub fn from_html(html: &str, plain: Option<&str>) -> Option<Clip> {
        from_html_within(html, plain, &Budget::standard())
    }
}

/// [`Clip::from_html`] against `budget`.
fn from_html_within(html: &str, plain: Option<&str>, budget: &Budget) -> Option<Clip> {
    let doc = import_html_str_within(html, budget).ok()?;
    let clip = crate::clip_rtf::clip_of(&doc, plain)?;
    (!clip.to_text().is_empty()).then_some(clip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Inline, Run};

    /// A run's text, whether it is bold and italic, and its link's target.
    type Shown = (String, bool, bool, Option<String>);

    /// Each run as [`Shown`], paragraph by paragraph.
    fn runs(clip: &Clip) -> Vec<Vec<Shown>> {
        fn push(out: &mut Vec<Shown>, r: &Run, link: Option<&str>) {
            out.push((
                r.text.clone(),
                r.props.bold,
                r.props.italic,
                link.map(str::to_string),
            ));
        }
        clip.paras
            .iter()
            .map(|p| {
                let mut out = Vec::new();
                for inline in p {
                    match inline {
                        Inline::Run(r) => push(&mut out, r, None),
                        Inline::Hyperlink(h) => {
                            for r in &h.runs {
                                push(&mut out, r, h.target.as_deref());
                            }
                        }
                        _ => {}
                    }
                }
                out
            })
            .collect()
    }

    #[test]
    fn bold_italic_and_a_link_come_through() {
        let html = "<html><body><p>Plain <b>bold</b> <i>it</i> <a href=\"https://example.com/\">link</a></p></body></html>";
        let clip = Clip::from_html(html, Some("Plain bold it link")).unwrap();
        assert_eq!(clip.to_text(), "Plain bold it link");
        let got = runs(&clip);
        assert_eq!(got.len(), 1, "{got:?}");
        let has = |text: &str, bold: bool, italic: bool, link: Option<&str>| {
            got[0]
                .iter()
                .any(|(t, b, i, l)| t == text && *b == bold && *i == italic && l.as_deref() == link)
        };
        assert!(has("bold", true, false, None), "{got:?}");
        assert!(has("it", false, true, None), "{got:?}");
        assert!(
            has("link", false, false, Some("https://example.com/")),
            "{got:?}"
        );
    }

    #[test]
    fn the_mark_comes_from_the_plain_text() {
        let html = "<p>one</p><p>two</p>";
        let partial = Clip::from_html(html, Some("one\ntwo")).unwrap();
        assert_eq!(partial.paras.len(), 2);
        assert_eq!(partial.to_text(), "one\ntwo");
        let through = Clip::from_html(html, Some("one\ntwo\n")).unwrap();
        assert_eq!(through.paras.len(), 3);
        assert!(through.paras[2].is_empty());
        assert_eq!(Clip::from_html(html, None).unwrap().paras.len(), 2);
    }

    #[test]
    fn a_table_row_is_one_tab_joined_paragraph() {
        let html =
            "<table><tr><td>a</td><td>M\u{fc}ller</td></tr><tr><td>1</td><td>2</td></tr></table>";
        let clip = Clip::from_html(html, None).unwrap();
        assert_eq!(clip.to_text(), "a\tM\u{fc}ller\n1\t2");
    }

    #[test]
    fn no_text_is_no_clip() {
        assert_eq!(Clip::from_html("", None), None);
        assert_eq!(Clip::from_html("<html><body></body></html>", None), None);
        assert_eq!(Clip::from_html("<p>   </p><img src=x>", Some("")), None);
    }

    /// The text is never re-read for a charset: a `charset=` in the words,
    /// or an Office page's windows-1252 meta over its UTF-8 bytes, leaves
    /// `\u{e9}` an `\u{e9}` (#1950 r1).
    #[test]
    fn no_charset_is_read_from_the_clipboards_text() {
        let html = "<p>Set charset=iso-8859-1 for caf\u{e9}</p>";
        let clip = Clip::from_html(html, None).unwrap();
        assert_eq!(clip.to_text(), "Set charset=iso-8859-1 for caf\u{e9}");
        let office = "<html><head><meta http-equiv=Content-Type content=\"text/html; charset=windows-1252\"></head><body><p>caf\u{e9}</p></body></html>";
        assert_eq!(
            Clip::from_html(office, None).unwrap().to_text(),
            "caf\u{e9}"
        );
    }
}
