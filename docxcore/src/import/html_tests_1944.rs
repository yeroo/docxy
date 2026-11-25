//! The page leftover of #1928 (#1944): an HTML page's own `@page` size and
//! margins give the import's section, and the body the reader lays out
//! in; a classic-Outlook mail, a paste and a `<style>` in the body keep
//! #1928's page.

use super::tests_1898::{pictures, png};
use super::*;
use crate::image::inline_extent;
use crate::model::{Block as DocBlock, PageGeom, Paragraph, Table};

/// `html` imported as a file: a 40 x 20 PNG for any src.
fn import(html: &str) -> HtmlImport {
    let mut resolve = |_: &str, _: usize| Some(png(40, 20));
    import_html_within(html.as_bytes(), &Budget::standard(), Some(&mut resolve)).unwrap()
}

fn doc(html: &str) -> Document {
    import(html).document
}

/// The import's final `w:sectPr`.
fn sect(d: &Document) -> String {
    d.trailing_section_properties()
        .expect("an HTML import has its own section")
        .raw
        .clone()
}

fn attr(xml: &str, el: &str, name: &str) -> Option<String> {
    let at = xml.find(&format!("<{el} "))?;
    let tag = &xml[at..at + xml[at..].find('>')?];
    let key = format!(" {name}=\"");
    let v = tag.find(&key)? + key.len();
    Some(tag[v..v + tag[v..].find('"')?].to_string())
}

/// The page: width, height, then top, right, bottom, left margins, then
/// header and footer.
fn page(d: &Document) -> [i64; 8] {
    let raw = sect(d);
    let a = |el: &str, n: &str| -> i64 {
        attr(&raw, el, n)
            .unwrap_or_else(|| panic!("{el} {n} in {raw}"))
            .parse()
            .unwrap()
    };
    [
        a("w:pgSz", "w:w"),
        a("w:pgSz", "w:h"),
        a("w:pgMar", "w:top"),
        a("w:pgMar", "w:right"),
        a("w:pgMar", "w:bottom"),
        a("w:pgMar", "w:left"),
        a("w:pgMar", "w:header"),
        a("w:pgMar", "w:footer"),
    ]
}

const DEFAULT: [i64; 8] = [12_240, 15_840, 225, 225, 225, 225, 225, 225];

/// A page with these `<style>` rules in its head and `body` as its body.
fn styled(css: &str, body: &str) -> String {
    format!("<html><head><style>{css}</style></head><body>{body}</body></html>")
}

/// Word's Web Page: 1" top and bottom, 1.25" sides, selected by its
/// `div.WordSection1` (the plan's criterion 2).
const WORD_CSS: &str = "@page WordSection1 {size:8.5in 11.0in; margin:1.0in 1.25in 1.0in 1.25in;}\
                        div.WordSection1 {page:WordSection1;}";

fn word_page(body: &str) -> String {
    styled(WORD_CSS, &format!("<div class=WordSection1>{body}</div>"))
}

fn tables(blocks: &[DocBlock]) -> Vec<&Table> {
    blocks
        .iter()
        .filter_map(|b| match b {
            DocBlock::Table(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn para<'d>(d: &'d Document, text: &str) -> &'d Paragraph {
    d.body
        .iter()
        .find_map(|b| match b {
            DocBlock::Paragraph(p) if p.plain_text() == text => Some(p),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no paragraph {text:?}: {:?}", d.body))
}

#[test]
fn a_word_page_keeps_its_own_margins_1944() {
    let d = doc(&word_page("<p>x</p>"));
    assert_eq!(page(&d), [12_240, 15_840, 1440, 1800, 1440, 1800, 720, 720]);
    let g = PageGeom::from_sect_pr(&sect(&d));
    assert_eq!(
        (g.w, g.ml, g.mr, g.mt, g.mb),
        (12_240, 1800, 1800, 1440, 1440)
    );
    // Portrait: no orientation written.
    assert_eq!(attr(&sect(&d), "w:pgSz", "w:orient"), None);
}

#[test]
fn the_body_follows_the_page_1944() {
    let body = 12_240 - 2 * 1800;
    assert_eq!(body, 8640);
    // A 100% outermost table fills the page's body.
    let d = doc(&word_page(
        "<table width=100%><tr><td>a</td><td>b</td></tr></table>",
    ));
    let t = tables(&d.body);
    assert_eq!(t[0].grid.iter().sum::<u32>(), body);
    // An 800 px picture is scaled down to the body.
    let got = import(&word_page("<p><img src=a.png width=800 height=400></p>"));
    let pics = pictures(&got.document.body);
    let (cx, cy) = inline_extent(pics[0]).unwrap();
    assert_eq!((cx / 635, cy / 635), (i64::from(body), i64::from(body) / 2));
    // A 400 px box with auto side margins is centred in it.
    let d = doc(&word_page(
        "<div style='width:400px;margin:0 auto'><p>x</p></div>",
    ));
    let p = para(&d, "x");
    let extra = (body as i32 - 6000) / 2;
    assert_eq!((p.props.indent, p.props.indent_right), (extra, extra));
}

#[test]
fn the_unnamed_page_applies_and_a_named_one_overrides_it_1944() {
    // No `page` property: the unnamed page alone.
    let d = doc(&styled(
        "@page {margin:0.5in} @page Other {margin:2in}",
        "<p>x</p>",
    ));
    assert_eq!(page(&d), [12_240, 15_840, 720, 720, 720, 720, 720, 720]);
    // A selected named page overrides the unnamed one per property: its
    // size here, the unnamed page's margins kept.
    let d = doc(&styled(
        "@page {margin:0.5in} @page Wide {size:landscape} div.w {page:Wide}",
        "<div class=w><p>x</p></div>",
    ));
    assert_eq!(page(&d), [15_840, 12_240, 720, 720, 720, 720, 720, 720]);
    // ... whatever the order of the rules, and names in any case.
    let d = doc(&styled(
        "div.w {page:wide} @page Wide {margin-left:2in} @page {margin:0.5in}",
        "<div class=w><p>x</p></div>",
    ));
    assert_eq!(page(&d), [12_240, 15_840, 720, 720, 720, 2880, 720, 720]);
    // `page: auto` or an unknown name selects none.
    for sel in ["auto", "Nowhere"] {
        let d = doc(&styled(
            &format!("@page P {{margin:2in}} div {{page:{sel}}}"),
            "<div><p>x</p></div>",
        ));
        assert_eq!(page(&d), DEFAULT, "{sel}");
    }
}

#[test]
fn page_sizes_read_lengths_keywords_and_orientation_1944() {
    let size = |v: &str| {
        let d = doc(&styled(&format!("@page {{size:{v}}}"), "<p>x</p>"));
        let raw = sect(&d);
        let p = page(&d);
        (p[0], p[1], attr(&raw, "w:pgSz", "w:orient"))
    };
    let land = Some("landscape".to_string());
    assert_eq!(size("8.5in 11in"), (12_240, 15_840, None));
    // Two lengths, wider than tall: written as Word stores a landscape page.
    assert_eq!(size("11in 8.5in"), (15_840, 12_240, land.clone()));
    // One length is a square.
    assert_eq!(size("6in"), (8640, 8640, None));
    assert_eq!(size("210mm 297mm"), (11_906, 16_838, None));
    assert_eq!(size("612pt 792pt"), (12_240, 15_840, None));
    assert_eq!(size("51pc 66pc"), (12_240, 15_840, None));
    assert_eq!(size("816px 1056px"), (12_240, 15_840, None));
    assert_eq!(size("21cm 29.7cm"), (11_906, 16_838, None));
    assert_eq!(size("auto"), (12_240, 15_840, None));
    assert_eq!(size("A4"), (11_906, 16_838, None));
    assert_eq!(size("a4 landscape"), (16_838, 11_906, land.clone()));
    assert_eq!(size("landscape a4"), (16_838, 11_906, land.clone()));
    assert_eq!(size("a5 portrait"), (8391, 11_906, None));
    assert_eq!(size("landscape"), (15_840, 12_240, land.clone()));
    assert_eq!(size("portrait"), (12_240, 15_840, None));
    assert_eq!(size("legal"), (12_240, 20_160, None));
    assert_eq!(size("ledger"), (15_840, 24_480, None));
    assert_eq!(size("letter landscape"), (15_840, 12_240, land));
    assert_eq!(size("a3"), (16_838, 23_811, None));
    assert_eq!(size("b4"), (14_173, 20_013, None));
    assert_eq!(size("b5"), (9978, 14_173, None));
    // Per side: one outside 1"..22" is ignored, the other kept.
    assert_eq!(size("8.5in 40in"), (12_240, 15_840, None));
    assert_eq!(size("6in 40in"), (8640, 15_840, None));
    assert_eq!(size("0.5in 9in"), (12_240, 12_960, None));
    // Unreadable: the whole value ignored.
    for v in [
        "50% 50%",
        "8.5in 11in 2in",
        "a4 a5",
        "landscape portrait",
        "8.5in landscape",
        "huge",
        "8.5in nope",
    ] {
        assert_eq!(size(v), (12_240, 15_840, None), "{v}");
    }
}

#[test]
fn margins_read_the_shorthand_longhands_and_word_s_header_1944() {
    let margins = |css: &str| page(&doc(&styled(&format!("@page {{{css}}}"), "<p>x</p>")));
    assert_eq!(
        margins("margin:1in"),
        [12_240, 15_840, 1440, 1440, 1440, 1440, 720, 720]
    );
    assert_eq!(
        margins("margin:1in 2in"),
        [12_240, 15_840, 1440, 2880, 1440, 2880, 720, 720]
    );
    assert_eq!(
        margins("margin:1in 2in 0.25in"),
        [12_240, 15_840, 1440, 2880, 360, 2880, 720, 360]
    );
    // Longhands override the shorthand, in order.
    assert_eq!(
        margins("margin:1in; margin-left:2cm; margin-top:36pt"),
        [12_240, 15_840, 720, 1440, 1440, 1134, 720, 720]
    );
    // Word's own header and footer distances.
    assert_eq!(
        margins("margin:1in; mso-header-margin:.3in; mso-footer-margin:0.4in"),
        [12_240, 15_840, 1440, 1440, 1440, 1440, 432, 576]
    );
    // A percentage, a negative or unreadable side is ignored alone.
    assert_eq!(
        margins("margin:1in; margin-left:10%; margin-right:-1in; margin-top:wide"),
        [12_240, 15_840, 1440, 1440, 1440, 1440, 720, 720]
    );
    assert_eq!(
        margins("margin:1in 10% 1in 2in"),
        [12_240, 15_840, 1440, 225, 1440, 2880, 720, 720]
    );
    // Margins leaving a body under 0.5" are all ignored; 0.5" itself is
    // kept.
    assert_eq!(
        margins("margin:4in"),
        [12_240, 15_840, 5760, 5760, 5760, 5760, 720, 720]
    );
    assert_eq!(margins("margin:4.1in"), DEFAULT);
    assert_eq!(margins("margin-top:7in; margin-bottom:4in"), DEFAULT);
    // On a page of its own size.
    assert_eq!(
        margins("size:3in; margin:1.4in"),
        [4320, 4320, 225, 225, 225, 225, 225, 225]
    );
    // Header and footer explicitly set survive margins being ignored.
    assert_eq!(
        margins("margin:6in; mso-header-margin:1in"),
        [12_240, 15_840, 225, 225, 225, 225, 1440, 225]
    );
}

#[test]
fn a_classic_outlook_mail_keeps_the_outlook_page_1944() {
    let mail = |generator: &str| {
        format!(
            "<html><head><meta name=Generator content=\"{generator}\">\
             <style>{WORD_CSS}</style></head><body><div class=WordSection1>\
             <table width=100%><tr><td>x</td></tr></table></div></body></html>"
        )
    };
    for g in [
        "Microsoft Word 15 (filtered medium)",
        "Microsoft Word 12 (Filtered Medium)",
    ] {
        let d = doc(&mail(g));
        assert_eq!(page(&d), DEFAULT, "{g}");
        assert_eq!(tables(&d.body)[0].grid, [html_table::BODY_TWIPS], "{g}");
    }
    // Word's own Web Page reads its page.
    for g in ["Microsoft Word 15 (filtered)", "Microsoft Word 15"] {
        let d = doc(&mail(g));
        assert_eq!(page(&d)[3], 1800, "{g}");
        assert_eq!(tables(&d.body)[0].grid, [8640], "{g}");
    }
    // Only the first Generator counts, and only a `name=generator` one.
    let d = doc(&mail("Microsoft Word 15").replace(
        "<style>",
        "<meta name=generator content='Microsoft Word 15 (filtered medium)'><style>",
    ));
    assert_eq!(page(&d)[3], 1800);
    let d = doc(&mail("Microsoft Word 15").replace(
        "<meta name=Generator",
        "<meta name=description content='(filtered medium)'><meta name=Generator",
    ));
    assert_eq!(page(&d)[3], 1800);
}

#[test]
fn the_outlook_fixture_s_generator_is_found_1944() {
    let html = include_str!("../../tests/fixtures/html/outlook-reply.html");
    let scan = prescan(html);
    assert!(scan.outlook_mail);
    assert!(scan.css.contains("@page WordSection1"));
    // Its sheet would select Word's 1" page: only the Generator keeps it.
    let sheet = StyleSheet::parse_split(&scan.css, scan.head_css, &Budget::standard());
    let word = html_table::Page::from_decls(&sheet.page_decls());
    assert_eq!((word.left, word.top), (1440, 1440));
    let d = doc(html);
    assert_eq!(page(&d), DEFAULT);
}

#[test]
fn a_paste_never_reads_the_page_1944() {
    let clip = "<html><head><meta name=Generator content=\"Microsoft Word 15\">\
         <style>@page WordSection1 {margin:1.0in} div.WordSection1 {page:WordSection1}</style>\
         </head><body><div class=WordSection1><table width=100%><tr><td>x</td></tr></table>\
         </div></body></html>";
    // As a file: the page's own body.
    assert_eq!(tables(&doc(clip).body)[0].grid, [12_240 - 2880]);
    // As a paste: the default body.
    let pasted = import_html_str_within(clip, &Budget::standard()).unwrap();
    assert_eq!(tables(&pasted.body)[0].grid, [html_table::BODY_TWIPS]);
}

#[test]
fn a_style_in_the_body_gives_no_page_1944() {
    // A quoted mail's `<style>` in the body: neither its `@page` nor its
    // `page` selects one.
    let quoted = format!(
        "<html><head></head><body><p>reply</p><style>{WORD_CSS}</style>\
         <div class=WordSection1><p>x</p></div></body></html>"
    );
    assert_eq!(page(&doc(&quoted)), DEFAULT);
    // The head's `@page` and a body `<style>` selecting it: still none.
    let split = "<html><head><style>@page P {margin:2in}</style></head><body>\
                 <style>div {page:P}</style><div><p>x</p></div></body></html>";
    assert_eq!(page(&doc(split)), DEFAULT);
    // The same rules in the head apply.
    let head = "<html><head><style>@page P {margin:2in}</style><style>div {page:P}</style>\
                </head><body><div><p>x</p></div></body></html>";
    assert_eq!(page(&doc(head))[2], 2880);
    // A body `<style>` still styles the text.
    let d = doc("<html><body><style>p {margin-left:1in}</style><p>x</p></body></html>");
    assert_eq!(para(&d, "x").props.indent, 1440);
    // A page with no `<body>` tag is all head.
    assert_eq!(
        page(&doc("<style>@page {margin:1in}</style><p>x</p>"))[2],
        1440
    );
}

#[test]
fn pages_without_a_page_rule_keep_the_default_1944() {
    for html in [
        "<p>x</p>",
        "<style>@page :first {margin:2in}</style><p>x</p>",
        "<style>@page WordSection1:first {margin:2in} div {page:WordSection1}</style><div>x</div>",
        "<style>@media print { @page {margin:2in} }</style><p>x</p>",
        "<style>@page A B {margin:2in}</style><p>x</p>",
        "<style>@pagex {margin:2in}</style><p>x</p>",
    ] {
        assert_eq!(page(&doc(html)), DEFAULT, "{html}");
    }
    // Any prefix of a page with one imports without a panic.
    let html = word_page("<table width=100%><tr><td>x</td></tr></table>");
    for k in 0..=html.len() {
        if html.is_char_boundary(k) {
            let _ = import_html(&html.as_bytes()[..k]);
        }
    }
}
