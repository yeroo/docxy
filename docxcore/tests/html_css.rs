//! HTML import with CSS (#1872): hand-written pages in the shapes real HTML
//! comes in (a newsletter, an Outlook reply, a Gmail message), checked for
//! the colours, fonts, sizes, spacing, indents and shading their CSS gives.

use docxcore::import::{import_html, paragraph_texts};
use docxcore::model::{Block, Document, Inline, Paragraph, Run};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/html")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn import(name: &str) -> Document {
    import_html(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Every paragraph, tables row-major, with whether it sits in a table cell.
fn paragraphs(doc: &Document) -> Vec<(&Paragraph, bool)> {
    fn walk<'a>(blocks: &'a [Block], in_cell: bool, out: &mut Vec<(&'a Paragraph, bool)>) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => out.push((p, in_cell)),
                Block::Table(t) => {
                    for cell in t.rows.iter().flat_map(|r| &r.cells) {
                        walk(&cell.blocks, true, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.body, false, &mut out);
    out
}

fn para<'a>(doc: &'a Document, text: &str) -> &'a Paragraph {
    paragraphs(doc)
        .into_iter()
        .map(|(p, _)| p)
        .find(|p| p.plain_text().contains(text))
        .unwrap_or_else(|| panic!("no paragraph holds {text:?}: {:?}", paragraph_texts(doc)))
}

fn run<'a>(doc: &'a Document, text: &str) -> &'a Run {
    paragraphs(doc)
        .into_iter()
        .flat_map(|(p, _)| &p.content)
        .find_map(|i| match i {
            Inline::Run(r) if r.text.contains(text) => Some(r),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no run holds {text:?}"))
}

fn shd(fill: &str) -> String {
    format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{fill}\"/>")
}

#[test]
fn a_newsletter_takes_its_style_sheet() {
    let d = import("newsletter.html");
    // The preheader is hidden text, not gone.
    let pre = run(&d, "This month");
    assert!(pre.props.vanish);
    assert!(!run(&d, "Spring catalogue is here").props.vanish);

    // `#headline`: the `@media` block (`!important` green, 22px) and the
    // style inside `<!--[if mso]>` do not apply.
    let h = run(&d, "Spring catalogue is here");
    assert_eq!(h.props.color.as_deref(), Some("0B5394"));
    assert_eq!(h.props.size_half_pts, Some(42), "28px = 21pt");
    assert!(h.props.bold);
    assert!(h.props.raw_props.is_empty(), "{:?}", h.props.raw_props);
    assert_eq!(
        h.props.font.as_deref(),
        Some("Segoe UI"),
        "from body's list"
    );
    let hp = para(&d, "Spring catalogue is here");
    assert_eq!(hp.props.style_id.as_deref(), Some("Heading1"));
    assert_eq!(hp.props.spacing.line, Some(510), "34px");
    assert_eq!(hp.props.spacing.line_rule.as_deref(), Some("atLeast"));
    assert_eq!(
        (hp.props.spacing.before, hp.props.spacing.after),
        (Some(0), Some(180))
    );

    // `.lead`, and `.content td > p` (more specific) for its margins.
    let lead = para(&d, "Dear customer");
    assert_eq!(lead.props.spacing.line, Some(360));
    assert_eq!(lead.props.spacing.after, Some(150));
    let dear = run(&d, "Dear customer");
    assert_eq!(dear.props.size_half_pts, Some(24));
    assert_eq!(dear.props.color.as_deref(), Some("333333"));
    let strong = run(&d, "new range");
    assert!(strong.props.bold);
    assert_eq!(strong.props.size_half_pts, Some(24));
    assert!(run(&d, "March").props.raw_props.contains(&shd("FFF2CC")));

    // `.content .note` (descendant) and the child rule together.
    let note = run(&d, "Prices include VAT");
    assert_eq!(note.props.color.as_deref(), Some("777777"));
    assert!(note.props.italic);
    assert_eq!(
        para(&d, "Prices include VAT").props.spacing.after,
        Some(150)
    );
    let code = run(&d, "SPRING26");
    assert_eq!(code.props.font.as_deref(), Some("Courier New"));
    assert_eq!(code.props.size_half_pts, Some(20), "13px = 9.75pt");

    // Cell paragraphs: neither the wrapper's grey nor the cell's padding.
    for (p, in_cell) in paragraphs(&d) {
        if in_cell {
            assert!(p.props.raw_props.is_empty(), "{:?}", p.props.raw_props);
            assert_eq!((p.props.indent, p.props.indent_right), (0, 0));
        }
    }

    // The footer sits on the wrapper's grey, centred and small.
    let foot = para(&d, "Northwind Traders");
    assert!(foot.props.raw_props.contains(&shd("F4F4F4")));
    assert_eq!(foot.props.align, docxcore::model::Align::Center);
    let ft = run(&d, "Northwind Traders");
    assert_eq!(ft.props.color.as_deref(), Some("999999"));
    assert_eq!(ft.props.size_half_pts, Some(17), "11px = 8.25pt");
    assert_eq!(
        run(&d, "Unsubscribe").props.color.as_deref(),
        Some("CC0000")
    );
    // `body`'s background is the page's: no paragraph outside the wrapper
    // is shaded.
    assert!(para(&d, "This month").props.raw_props.is_empty());
}

#[test]
fn an_outlook_reply_keeps_its_colours_and_quote_indents() {
    let d = import("outlook-reply.html");
    assert!(
        !paragraph_texts(&d)
            .iter()
            .any(|t| t.contains("panose") || t.contains("Cambria")),
        "{:?}",
        paragraph_texts(&d)
    );
    let thanks = run(&d, "Thanks, Maria");
    assert_eq!(thanks.props.color.as_deref(), Some("1F497D"));
    assert_eq!(thanks.props.font.as_deref(), Some("Calibri"));
    assert_eq!(thanks.props.size_half_pts, Some(22));
    let jon = run(&d, "Jon");
    assert!(jon.props.bold);
    assert_eq!(jon.props.color.as_deref(), Some("1F497D"));
    assert_eq!(run(&d, "Finance").props.color, None, "windowtext");
    let normal = para(&d, "Thanks, Maria");
    assert_eq!(
        (normal.props.spacing.before, normal.props.spacing.after),
        (Some(0), Some(0))
    );
    assert_eq!(normal.props.indent, 0);

    // The quoted header: inside the `border-left` div, 4pt of padding.
    let from = para(&d, "From:");
    assert_eq!(from.props.indent, 80);
    assert_eq!(from.props.indent_right, 0);
    assert!(run(&d, "From:").props.bold);
    assert_eq!(run(&d, "From:").props.size_half_pts, Some(22));
    // The blockquote: its default 40px each side, on the div's padding.
    let quoted = para(&d, "Q1 figures are attached");
    assert_eq!(quoted.props.indent, 80 + 600);
    assert_eq!(quoted.props.indent_right, 600);
    assert_eq!(
        run(&d, "Q1 figures are attached").props.font.as_deref(),
        Some("Calibri")
    );
}

#[test]
fn a_plain_message_and_its_gmail_quote() {
    let d = import("plain-message.html");
    let texts = paragraph_texts(&d);
    assert!(texts.iter().any(|t| t == "Hi Sam,"), "{texts:?}");
    assert!(run(&d, "Friday").props.bold);
    assert_eq!(run(&d, "Bring").props.color.as_deref(), Some("FF0000"));
    assert_eq!(run(&d, "the notes").props.color, None);
    let attr = para(&d, "wrote:");
    assert_eq!(attr.props.indent, 0);
    // `margin:0 0 0 0.8ex; padding-left:1ex`: 1.8ex at 12pt is 216 twips,
    // and the right margin of 0 replaces the blockquote's default.
    let quoted = para(&d, "free for lunch");
    assert_eq!(quoted.props.indent, 216);
    assert_eq!(quoted.props.indent_right, 0);
    assert_eq!(run(&d, "-- Sam").props.color.as_deref(), Some("888888"));
}

#[test]
fn every_prefix_of_every_fixture_imports_without_panicking() {
    for name in [
        "newsletter.html",
        "outlook-reply.html",
        "plain-message.html",
    ] {
        let bytes = fixture(name);
        for i in 0..=bytes.len() {
            let _ = import_html(&bytes[..i]);
        }
    }
}
