//! A whitespace-only paragraph (#1973): spaces or a tab in a look a browser
//! draws across them (underline, strike, highlight, shading) are a blank
//! to fill in and keep their run; spaces in any other look stay an empty
//! paragraph, whose mark takes the line's font and size.

use super::*;
use crate::model::{Block as DocBlock, Paragraph, Run};

fn doc(html: &str) -> Document {
    import_html(html.as_bytes()).unwrap()
}

fn paras(d: &Document) -> Vec<Paragraph> {
    d.body
        .iter()
        .filter_map(|b| match b {
            DocBlock::Paragraph(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

/// The first paragraph of `html`.
fn first(html: &str) -> Paragraph {
    paras(&doc(html)).remove(0)
}

/// The one run of `p`.
fn one_run(p: &Paragraph) -> Run {
    match p.content.as_slice() {
        [Inline::Run(r)] => r.clone(),
        other => panic!("one run: {other:?}"),
    }
}

/// The bookmark names in `p`, in order.
fn marks(p: &Paragraph) -> Vec<String> {
    p.content
        .iter()
        .filter_map(|i| match i {
            Inline::Raw(r) if r.starts_with("<w:bookmarkStart ") => {
                let name = r.split("w:name=\"").nth(1)?;
                Some(name.trim_end_matches("\"/>").to_string())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn underlined_spaces_alone_keep_their_run_1973() {
    let p = first(
        "<p><span style=\"font-family:Georgia;text-decoration:underline\">\
         &nbsp;&nbsp;&nbsp;</span></p><p>x</p>",
    );
    let r = one_run(&p);
    assert_eq!(r.text, "\u{a0}\u{a0}\u{a0}");
    assert!(r.props.underline, "{r:?}");
    assert_eq!(r.props.font.as_deref(), Some("Georgia"));
}

#[test]
fn plain_and_unstyled_spaces_alone_stay_empty_1973() {
    for html in [
        "<p>&nbsp;</p>",
        "<p><span>&nbsp;</span></p>",
        "<p><span style=\"font-family:Georgia\">&nbsp;</span></p>",
        "<p class=MsoNormal><o:p>&nbsp;</o:p></p>",
        "<p><b>&nbsp; &nbsp;</b></p>",
    ] {
        let p = first(&format!("{html}<p>x</p>"));
        assert!(p.content.is_empty(), "{html}: {:?}", p.content);
    }
    // Plain spaces between blocks still make no paragraph.
    let d = doc("<div>\n  <p>a</p>\n  \n<p>b</p>\n</div>");
    assert_eq!(paras(&d).len(), 2, "{:?}", paras(&d));
}

#[test]
fn highlighted_struck_and_shaded_spaces_alone_keep_their_run_1973() {
    let r = one_run(&first(
        "<p><span style=\"mso-highlight:yellow\">&nbsp;&nbsp;</span></p>",
    ));
    assert_eq!(r.props.highlight.as_deref(), Some("yellow"));
    let r = one_run(&first(
        "<p><span style=\"text-decoration:line-through\">&nbsp;&nbsp;</span></p>",
    ));
    assert!(r.props.strike, "{r:?}");
    let r = one_run(&first(
        "<p><span style=\"background-color:#FFCC00\">&nbsp;&nbsp;</span></p>",
    ));
    assert!(
        r.props.raw_props.iter().any(|x| x.starts_with("<w:shd")),
        "{r:?}"
    );
    // A block's underline is the runs' too, and a browser draws it.
    let r = one_run(&first(
        "<p style=\"text-decoration:underline\">&nbsp;&nbsp;</p>",
    ));
    assert!(r.props.underline, "{r:?}");
}

#[test]
fn hidden_spaces_alone_stay_empty_1973() {
    let p = first(
        "<p><span style=\"display:none;text-decoration:underline\">&nbsp;&nbsp;</span></p><p>x</p>",
    );
    assert!(p.content.is_empty(), "{:?}", p.content);
}

/// Word's own empty paragraph in an underlined look: its `&nbsp;` has the
/// underline turned off, so a browser draws no line and it stays empty.
#[test]
fn underline_turned_off_on_the_space_stays_empty_1973() {
    for html in [
        "<p style=\"text-decoration:underline\"><span style=\"text-decoration:none\">&nbsp;</span></p>",
        "<p class=MsoNormal><u><o:p><span style='text-decoration:none'>&nbsp;</span></o:p></u></p>",
    ] {
        let p = first(&format!("{html}<p>x</p>"));
        assert!(p.content.is_empty(), "{html}: {:?}", p.content);
    }
}

#[test]
fn an_underlined_tab_alone_keeps_its_tab_1973() {
    for html in [
        "<p><span style=\"text-decoration:underline\"><span style=\"mso-tab-count:1\">\
         &nbsp;&nbsp;&nbsp;</span></span></p>",
        "<p><span style=\"text-decoration:underline;white-space:pre\">\t</span></p>",
    ] {
        let p = first(&format!("{html}<p>x</p>"));
        match p.content.as_slice() {
            [Inline::Tab(t)] => assert!(t.underline, "{html}: {t:?}"),
            other => panic!("{html}: one underlined tab: {other:?}"),
        }
    }
    // A plain tab alone is still an empty paragraph.
    let p = first("<p><span style=\"mso-tab-count:1\">&nbsp;&nbsp;</span></p><p>x</p>");
    assert!(p.content.is_empty(), "{:?}", p.content);
}

/// A kept blank holds its bookmark (#1912), as any paragraph with content.
#[test]
fn a_blank_keeps_its_bookmark_1973() {
    // Only an `id` the page links to is a bookmark.
    let d = doc(
        "<p id=\"blank\"><u>&nbsp;&nbsp;&nbsp;</u></p><p>x</p><p><a href=\"#blank\">to</a></p>",
    );
    let ps = paras(&d);
    assert_eq!(ps.len(), 3, "{ps:?}");
    assert_eq!(marks(&ps[0]), ["blank"]);
    assert!(marks(&ps[1]).is_empty(), "{:?}", ps[1].content);
    assert_eq!(
        ps[0]
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) => Some(r.text.as_str()),
                _ => None,
            })
            .collect::<String>(),
        "\u{a0}".repeat(3)
    );
}

#[test]
fn an_empty_lines_size_is_its_marks_1973() {
    let p = first("<p><span style=\"font-size:20pt\">&nbsp;</span></p><p>x</p>");
    assert!(p.content.is_empty(), "{:?}", p.content);
    let mark = p.props.mark_run_props().expect("a mark");
    assert_eq!(mark.size_half_pts, Some(40));
    // The tallest run sizes the line, in its font.
    let p = first(
        "<p><span style=\"font-size:8pt\">&nbsp;</span>\
         <span style=\"font-size:20pt;font-family:Georgia\">&nbsp;</span>\
         <span style=\"font-size:20pt\">&nbsp;</span></p><p>x</p>",
    );
    let mark = p.props.mark_run_props().expect("a mark");
    assert_eq!(mark.size_half_pts, Some(40));
    assert_eq!(mark.font.as_deref(), Some("Georgia"));
}

/// A hidden span is no part of the line: the block's mark stays.
#[test]
fn a_hidden_sized_span_leaves_the_mark_1973() {
    let p = first("<p><span style=\"display:none;font-size:20pt\">&nbsp;</span></p><p>x</p>");
    assert!(p.content.is_empty(), "{:?}", p.content);
    let mark = p.props.mark_run_props().unwrap_or_default();
    assert!(!mark.vanish, "{mark:?}");
    assert_ne!(mark.size_half_pts, Some(40), "{mark:?}");
}

/// A list item's mark is its marker's look (#1979), not its empty line's.
#[test]
fn a_list_items_mark_is_unchanged_1973() {
    let plain = first("<ol><li>&nbsp;</li></ol><p>x</p>");
    let sized = first("<ol><li><span style=\"font-size:20pt\">&nbsp;</span></li></ol><p>x</p>");
    assert!(sized.props.num_id.is_some(), "{:?}", sized.props);
    assert_eq!(sized.props.raw_props, plain.props.raw_props);
}

/// A typed list marker's spacer is cut with it, not an underlined blank
/// right after it (r1 m4).
#[test]
fn a_typed_markers_gap_stops_at_a_blank_1973() {
    let d = doc(
        "<p class=MsoListParagraph>1.<span style='font:7.0pt \"Times New Roman\"'>&nbsp;&nbsp;&nbsp;</span>\
         <u>&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;</u></p><p>x</p>",
    );
    let p = paras(&d).remove(0);
    assert!(p.props.num_id.is_some(), "{:?}", p.props);
    let r = one_run(&p);
    assert_eq!(r.text, "\u{a0}".repeat(5));
    assert!(r.props.underline, "{r:?}");
}

/// The mark size of `html`'s first paragraph, which must be empty.
fn mark_size(html: &str) -> Option<u32> {
    let p = first(html);
    assert!(p.content.is_empty(), "{html}: {:?}", p.content);
    p.props.mark_run_props().and_then(|m| m.size_half_pts)
}

/// In standards mode a line is never shorter than its block's own font
/// (the strut); quirks mode has none, so a smaller span sizes the line
/// (r1 m3).
#[test]
fn an_empty_lines_size_is_at_least_its_blocks_in_standards_mode_1973() {
    let body =
        "<p style=\"font-size:20pt\"><span style=\"font-size:8pt\">&nbsp;</span></p><p>x</p>";
    assert_eq!(mark_size(&format!("<!DOCTYPE html>{body}")), Some(40));
    assert_eq!(mark_size(body), Some(16), "quirks mode");
    // A larger span still sizes the line in standards mode.
    assert_eq!(
        mark_size(
            "<!DOCTYPE html><p style=\"font-size:8pt\"><span style=\"font-size:20pt\">&nbsp;</span></p><p>x</p>"
        ),
        Some(40)
    );
    // Outlook's empty line: its mail has no doctype, so quirks mode, and
    // the span's 8pt sizes it; with one, the class's 11pt stands.
    let outlook = "<style>p.MsoNormal{font-size:11.0pt}</style>\
                   <p class=MsoNormal><span style='font-size:8.0pt'><o:p>&nbsp;</o:p></span></p><p>x</p>";
    assert_eq!(mark_size(outlook), Some(16), "quirks mode");
    assert_eq!(mark_size(&format!("<!DOCTYPE html>{outlook}")), Some(22));
}

/// Gmail's empty line, a `<br>` alone in its look: the mark takes its
/// size (r1 m5).
#[test]
fn a_lone_breaks_size_is_its_marks_1973() {
    assert_eq!(
        mark_size("<div><span style=\"font-size:20pt\"><br></span></div><div>x</div>"),
        Some(40)
    );
}
