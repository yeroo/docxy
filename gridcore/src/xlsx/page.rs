//! Page setup in the worksheet part: read into [`PageSetup`] at load, and
//! patched back at save.
//!
//! The save never regenerates these elements. It compares the model with
//! what was loaded and rewrites only the attributes (and header/footer
//! children) that differ, so `r:id`, printer DPI, `copies` and any spelling
//! it didn't touch stay byte-for-byte. A missing element is created at its
//! `CT_Worksheet` position; an element left with nothing in it goes, except
//! `<pageMargins>`, whose attributes are all required.

use opccore::xml::{Event, XmlParser};

use super::{
    decode, element_children, esc_text, put_worksheet_child, remove_worksheet_child, set_tag_attr,
    tag_end, worksheet_child_span, worksheet_children,
};
use crate::print::setup::{
    CellComments, HeaderFooter, HfSlot, Orientation, PageOrder, PageSetup, PrintErrors, xml_bool,
};
use crate::sheet::OutlineSettings;

/// The elements [`read_page_setup`] reads. A part naming none of them has
/// the default page setup, and isn't walked.
const NAMES: [&str; 5] = [
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "pageSetUpPr",
];

/// The page setup a worksheet part holds. Only the sheet's own top-level
/// elements count: a `<customSheetView>` carries its own copies.
pub(super) fn read_page_setup(xml: &str) -> PageSetup {
    let mut s = PageSetup::default();
    if !NAMES.iter().any(|n| xml.contains(n)) {
        return s;
    }
    let walk = worksheet_children(xml);
    for c in &walk.children {
        let frag = &xml[c.start..c.end];
        match c.local {
            "printOptions" => {
                let a = StartTag::read(frag);
                s.grid_lines = a.flag("gridLines").unwrap_or(false);
                s.headings = a.flag("headings").unwrap_or(false);
                s.h_centered = a.flag("horizontalCentered").unwrap_or(false);
                s.v_centered = a.flag("verticalCentered").unwrap_or(false);
            }
            "pageMargins" => {
                let a = StartTag::read(frag);
                let m = &mut s.margins;
                for (name, field) in [
                    ("left", &mut m.left),
                    ("right", &mut m.right),
                    ("top", &mut m.top),
                    ("bottom", &mut m.bottom),
                    ("header", &mut m.header),
                    ("footer", &mut m.footer),
                ] {
                    if let Some(v) = a.num::<f64>(name).filter(|v| v.is_finite()) {
                        *field = v;
                    }
                }
            }
            "pageSetup" => {
                let a = StartTag::read(frag);
                if let Some(v) = a.num("paperSize") {
                    s.paper_size = v;
                }
                if let Some(v) = a.num("scale") {
                    s.scale = v;
                }
                if let Some(v) = a.num("fitToWidth") {
                    s.fit_width = v;
                }
                if let Some(v) = a.num("fitToHeight") {
                    s.fit_height = v;
                }
                if a.flag("useFirstPageNumber") == Some(true) {
                    s.first_page_number = Some(a.num("firstPageNumber").unwrap_or(1));
                }
                if let Some(v) = a.get("orientation").and_then(Orientation::parse) {
                    s.orientation = v;
                }
                if let Some(v) = a.get("pageOrder").and_then(PageOrder::parse) {
                    s.page_order = v;
                }
                if let Some(v) = a.get("cellComments").and_then(CellComments::parse) {
                    s.cell_comments = v;
                }
                if let Some(v) = a.get("errors").and_then(PrintErrors::parse) {
                    s.errors = v;
                }
                s.black_and_white = a.flag("blackAndWhite").unwrap_or(false);
                s.draft = a.flag("draft").unwrap_or(false);
            }
            "headerFooter" => s.header_footer = read_header_footer(frag),
            "sheetPr" => {
                for (name, cs, _) in element_children(frag) {
                    if name == "pageSetUpPr" {
                        let a = StartTag::read(&frag[cs..]);
                        s.fit_to_page = a.flag("fitToPage").unwrap_or(false);
                    }
                }
            }
            _ => {}
        }
    }
    s
}

fn read_header_footer(frag: &str) -> HeaderFooter {
    let a = StartTag::read(frag);
    let mut hf = HeaderFooter {
        different_odd_even: a.flag("differentOddEven").unwrap_or(false),
        different_first: a.flag("differentFirst").unwrap_or(false),
        scale_with_doc: a.flag("scaleWithDoc").unwrap_or(true),
        align_with_margins: a.flag("alignWithMargins").unwrap_or(true),
        ..HeaderFooter::default()
    };
    for (name, cs, ce) in element_children(frag) {
        if let Some(slot) = HfSlot::from_element(&name) {
            *hf.slot_mut(slot) = Some(element_text(&frag[cs..ce]));
        }
    }
    hf
}

/// The decoded text content of one element.
fn element_text(frag: &str) -> String {
    let mut out = String::new();
    let mut p = XmlParser::new(frag);
    loop {
        match p.next() {
            Event::Text => out.push_str(&decode(p.text())),
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// The attributes of a fragment's first start tag.
struct StartTag<'a> {
    attrs: Vec<(&'a str, &'a str)>,
}

impl<'a> StartTag<'a> {
    fn read(frag: &'a str) -> Self {
        let mut p = XmlParser::new(frag);
        loop {
            match p.next() {
                Event::Start => {
                    return StartTag {
                        attrs: p.attrs().iter().map(|a| (a.name, a.value)).collect(),
                    };
                }
                Event::Eof => return StartTag { attrs: Vec::new() },
                _ => {}
            }
        }
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.attrs.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }

    fn flag(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(xml_bool)
    }

    fn num<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.get(name).and_then(|v| v.trim().parse().ok())
    }
}

/// Write `now` into the part where it differs from `was` (what the part held
/// at load). Equal models leave the part byte-for-byte.
pub(super) fn set_page_setup(xml: &str, now: &PageSetup, was: &PageSetup) -> String {
    if now == was {
        return xml.to_string();
    }
    let mut out = xml.to_string();

    // <printOptions>: booleans, default false.
    let mut edits = Vec::new();
    for (attr, n, w) in [
        ("gridLines", now.grid_lines, was.grid_lines),
        ("headings", now.headings, was.headings),
        ("horizontalCentered", now.h_centered, was.h_centered),
        ("verticalCentered", now.v_centered, was.v_centered),
    ] {
        if n != w {
            edits.push((attr, n.then(|| "1".to_string())));
        }
    }
    out = patch_attrs(out, "printOptions", &edits, true);

    // <pageMargins>: every attribute required, so a new element gets all six
    // and the element never goes.
    let (m, mw) = (&now.margins, &was.margins);
    let margins = [
        ("left", m.left, mw.left),
        ("right", m.right, mw.right),
        ("top", m.top, mw.top),
        ("bottom", m.bottom, mw.bottom),
        ("header", m.header, mw.header),
        ("footer", m.footer, mw.footer),
    ];
    if margins.iter().any(|(_, n, w)| n != w) {
        if worksheet_child_span(&out, "pageMargins").is_some() {
            let edits: Vec<_> = margins
                .iter()
                .filter(|(_, n, w)| n != w)
                .map(|(a, n, _)| (*a, Some(n.to_string())))
                .collect();
            out = patch_attrs(out, "pageMargins", &edits, false);
        } else {
            let attrs: String = margins
                .iter()
                .map(|(a, n, _)| format!(" {a}=\"{n}\""))
                .collect();
            out = put_worksheet_child(
                &out,
                "pageMargins",
                &format!("<pageMargins{attrs}/>"),
                None,
                false,
            );
        }
    }

    // <pageSetup>: an attribute at its schema default goes.
    let mut edits: Vec<(&str, Option<String>)> = Vec::new();
    let mut num = |attr, n: u32, w: u32, default: u32| {
        if n != w {
            edits.push((attr, (n != default).then(|| n.to_string())));
        }
    };
    num("paperSize", now.paper_size, was.paper_size, 1);
    num("scale", now.scale, was.scale, 100);
    num("fitToWidth", now.fit_width, was.fit_width, 1);
    num("fitToHeight", now.fit_height, was.fit_height, 1);
    let mut word = |attr, n: &'static str, w: &'static str, default: &str| {
        if n != w {
            edits.push((attr, (n != default).then(|| n.to_string())));
        }
    };
    word(
        "orientation",
        now.orientation.as_str(),
        was.orientation.as_str(),
        "default",
    );
    word(
        "pageOrder",
        now.page_order.as_str(),
        was.page_order.as_str(),
        "downThenOver",
    );
    word(
        "cellComments",
        now.cell_comments.as_str(),
        was.cell_comments.as_str(),
        "none",
    );
    word(
        "errors",
        now.errors.as_str(),
        was.errors.as_str(),
        "displayed",
    );
    for (attr, n, w) in [
        ("blackAndWhite", now.black_and_white, was.black_and_white),
        ("draft", now.draft, was.draft),
    ] {
        if n != w {
            edits.push((attr, n.then(|| "1".to_string())));
        }
    }
    if now.first_page_number != was.first_page_number {
        match now.first_page_number {
            // Auto: `firstPageNumber` stays as it was; without
            // `useFirstPageNumber` it means nothing.
            None => edits.push(("useFirstPageNumber", None)),
            Some(n) => {
                edits.push(("firstPageNumber", Some(n.to_string())));
                edits.push(("useFirstPageNumber", Some("1".to_string())));
            }
        }
    }
    out = patch_attrs(out, "pageSetup", &edits, true);

    out = patch_header_footer(out, &now.header_footer, &was.header_footer);
    patch_fit_to_page(out, now.fit_to_page, was.fit_to_page)
}

/// Set (`Some`) or remove (`None`) attributes on the top-level `<tag>`,
/// creating it when something is set and there is none. With `drop_bare`,
/// an element left with no attributes and no content goes.
fn patch_attrs(
    mut xml: String,
    tag: &str,
    edits: &[(&str, Option<String>)],
    drop_bare: bool,
) -> String {
    if edits.is_empty() {
        return xml;
    }
    let Some((start, _)) = worksheet_child_span(&xml, tag) else {
        let attrs: String = edits
            .iter()
            .filter_map(|(a, v)| v.as_ref().map(|v| format!(" {a}=\"{v}\"")))
            .collect();
        if attrs.is_empty() {
            return xml;
        }
        return put_worksheet_child(&xml, tag, &format!("<{tag}{attrs}/>"), None, false);
    };
    for (attr, value) in edits {
        xml = set_tag_attr(&xml, start, attr, value.as_deref());
    }
    if drop_bare {
        if let Some((s, e)) = worksheet_child_span(&xml, tag) {
            if is_bare(&xml[s..e]) {
                xml = remove_worksheet_child(&xml, tag);
            }
        }
    }
    xml
}

/// An element with no attributes (namespace declarations count) and nothing
/// but whitespace inside.
fn is_bare(frag: &str) -> bool {
    let mut p = XmlParser::new(frag);
    if p.next() != Event::Start || !p.attrs().is_empty() || !p.namespace_attrs().is_empty() {
        return false;
    }
    loop {
        match p.next() {
            Event::Text if p.text().trim().is_empty() => {}
            Event::End => return true,
            _ => return false,
        }
    }
}

/// The `prefix:` (or nothing) of the element starting at `start`.
fn prefix_at(xml: &str, start: usize) -> &str {
    let rest = &xml[start + 1..];
    let name_len = rest
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(rest.len());
    match rest[..name_len].rfind(':') {
        Some(i) => &rest[..=i],
        None => "",
    }
}

/// Open a self-closing element at `start` (`<a x="1"/>` → `<a x="1"></a>`).
/// Returns where its content goes. A non-empty element is left alone.
fn open_up(xml: &mut String, start: usize) -> usize {
    let end = tag_end(xml, start);
    if !xml[..end].ends_with("/>") {
        return end;
    }
    let rest = &xml[start + 1..];
    let name_len = rest
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(rest.len());
    let qname = rest[..name_len].to_string();
    // Drop the `/` (and any whitespace before it).
    let slash = xml[..end - 1]
        .trim_end_matches([' ', '\t', '\r', '\n'])
        .len()
        - 1;
    let tail = format!("></{qname}>");
    xml.replace_range(slash..end, &tail);
    slash + 1
}

/// Sync `<headerFooter>`: its four flags as attributes (each dropped at its
/// default), and the six strings as child elements in their sequence order.
/// An unchanged child is left as it is.
fn patch_header_footer(mut xml: String, now: &HeaderFooter, was: &HeaderFooter) -> String {
    if now == was {
        return xml;
    }
    let mut edits: Vec<(&str, Option<String>)> = Vec::new();
    for (attr, n, w, default) in [
        (
            "differentOddEven",
            now.different_odd_even,
            was.different_odd_even,
            false,
        ),
        (
            "differentFirst",
            now.different_first,
            was.different_first,
            false,
        ),
        ("scaleWithDoc", now.scale_with_doc, was.scale_with_doc, true),
        (
            "alignWithMargins",
            now.align_with_margins,
            was.align_with_margins,
            true,
        ),
    ] {
        if n != w {
            edits.push((
                attr,
                (n != default).then(|| if n { "1" } else { "0" }.to_string()),
            ));
        }
    }
    let changed: Vec<HfSlot> = HfSlot::ALL
        .into_iter()
        .filter(|&s| now.get(s) != was.get(s))
        .collect();

    let Some((start, _)) = worksheet_child_span(&xml, "headerFooter") else {
        let attrs: String = edits
            .iter()
            .filter_map(|(a, v)| v.as_ref().map(|v| format!(" {a}=\"{v}\"")))
            .collect();
        let children: String = HfSlot::ALL
            .into_iter()
            .filter_map(|s| now.get(s).map(|t| hf_child("", s, t)))
            .collect();
        if attrs.is_empty() && children.is_empty() {
            return xml;
        }
        let block = if children.is_empty() {
            format!("<headerFooter{attrs}/>")
        } else {
            format!("<headerFooter{attrs}>{children}</headerFooter>")
        };
        return put_worksheet_child(&xml, "headerFooter", &block, None, false);
    };

    for (attr, value) in &edits {
        xml = set_tag_attr(&xml, start, attr, value.as_deref());
    }
    if !changed.is_empty() {
        let prefix = prefix_at(&xml, start).to_string();
        if changed.iter().any(|&s| now.get(s).is_some()) {
            open_up(&mut xml, start);
        }
        let Some((s, e)) = worksheet_child_span(&xml, "headerFooter") else {
            return xml;
        };
        let frag = &xml[s..e];
        let rank = |slot: HfSlot| HfSlot::ALL.iter().position(|&x| x == slot);
        // The element's content: after its start tag, before its end tag.
        let content_start = tag_end(frag, 0);
        let content_end = if frag.ends_with("/>") {
            frag.len()
        } else {
            frag.rfind("</").unwrap_or(frag.len())
        }
        .max(content_start);
        let children = element_children(frag);
        let present = |slot: HfSlot| {
            children
                .iter()
                .any(|(n, _, _)| HfSlot::from_element(n) == Some(slot))
        };
        // New children, in sequence order, each placed before the first
        // existing child that ranks after it, or at the end.
        let mut pending: std::collections::VecDeque<HfSlot> = HfSlot::ALL
            .into_iter()
            .filter(|&slot| changed.contains(&slot) && now.get(slot).is_some() && !present(slot))
            .collect();
        // One pass over the children in document order: text between them
        // and unchanged children are copied as they are, a changed child is
        // replaced (or dropped), and new ones go in where they rank.
        let mut content = String::new();
        let mut pos = content_start;
        for (name, cs, ce) in children.iter().map(|(n, a, b)| (n.as_str(), *a, *b)) {
            content.push_str(&frag[pos..cs]);
            let own = HfSlot::from_element(name);
            if let Some(r) = own.and_then(rank) {
                while pending.front().is_some_and(|&p| rank(p) < Some(r)) {
                    let p = pending.pop_front().expect("checked");
                    content.push_str(&hf_child(&prefix, p, now.get(p).unwrap_or_default()));
                }
            }
            match own.filter(|slot| changed.contains(slot)) {
                Some(slot) => {
                    if let Some(t) = now.get(slot) {
                        content.push_str(&hf_child(&prefix, slot, t));
                    }
                }
                None => content.push_str(&frag[cs..ce]),
            }
            pos = ce;
        }
        content.push_str(&frag[pos..content_end]);
        for p in pending {
            content.push_str(&hf_child(&prefix, p, now.get(p).unwrap_or_default()));
        }
        let frag = format!(
            "{}{content}{}",
            &frag[..content_start],
            &frag[content_end..]
        );
        xml.replace_range(s..e, &frag);
    }
    if let Some((s, e)) = worksheet_child_span(&xml, "headerFooter") {
        if is_bare(&xml[s..e]) {
            xml = remove_worksheet_child(&xml, "headerFooter");
        }
    }
    xml
}

fn hf_child(prefix: &str, slot: HfSlot, text: &str) -> String {
    let name = slot.element();
    format!("<{prefix}{name}>{}</{prefix}{name}>", esc_text(text))
}

/// Sync `<sheetPr><pageSetUpPr fitToPage>`. `pageSetUpPr` follows `tabColor`
/// and `outlinePr` in `CT_SheetPr`.
fn patch_fit_to_page(mut xml: String, now: bool, was: bool) -> String {
    if now == was {
        return xml;
    }
    let Some((start, end)) = worksheet_child_span(&xml, "sheetPr") else {
        if !now {
            return xml;
        }
        return put_worksheet_child(
            &xml,
            "sheetPr",
            "<sheetPr><pageSetUpPr fitToPage=\"1\"/></sheetPr>",
            None,
            false,
        );
    };
    let prefix = prefix_at(&xml, start).to_string();
    let children = element_children(&xml[start..end]);
    match children.iter().find(|(n, _, _)| n == "pageSetUpPr") {
        Some(&(_, cs, _)) => {
            let at = start + cs;
            xml = set_tag_attr(&xml, at, "fitToPage", now.then_some("1"));
            if !now {
                // A `pageSetUpPr` with nothing left, and then a bare
                // `sheetPr`, go too.
                let (s, e) = worksheet_child_span(&xml, "sheetPr").unwrap_or((start, start));
                let bare = element_children(&xml[s..e])
                    .into_iter()
                    .find(|(n, _, _)| n == "pageSetUpPr")
                    .filter(|&(_, cs, ce)| is_bare(&xml[s + cs..s + ce]));
                if let Some((_, cs, ce)) = bare {
                    xml.replace_range(s + cs..s + ce, "");
                    if let Some((s, e)) = worksheet_child_span(&xml, "sheetPr") {
                        if is_bare(&xml[s..e]) {
                            xml = remove_worksheet_child(&xml, "sheetPr");
                        }
                    }
                }
            }
        }
        None if now => {
            let after = children
                .iter()
                .filter(|(n, _, _)| n == "tabColor" || n == "outlinePr")
                .map(|&(_, _, ce)| start + ce)
                .max();
            let content = open_up(&mut xml, start);
            let at = after.unwrap_or(content);
            xml.insert_str(at, &format!("<{prefix}pageSetUpPr fitToPage=\"1\"/>"));
        }
        None => {}
    }
    xml
}

/// `<sheetPr><outlinePr summaryBelow summaryRight>`. Both default to true.
pub(super) fn read_outline_pr(xml: &str) -> OutlineSettings {
    let mut s = OutlineSettings::default();
    if !xml.contains("outlinePr") {
        return s;
    }
    let Some((start, end)) = worksheet_child_span(xml, "sheetPr") else {
        return s;
    };
    let frag = &xml[start..end];
    for (name, cs, _) in element_children(frag) {
        if name == "outlinePr" {
            let a = StartTag::read(&frag[cs..]);
            s.summary_below = a.flag("summaryBelow").unwrap_or(true);
            s.summary_right = a.flag("summaryRight").unwrap_or(true);
        }
    }
    s
}

/// Sync `<sheetPr><outlinePr>` where `now` differs from `was` (what the part
/// held at load), so an untouched element stays byte-for-byte. `outlinePr`
/// follows `tabColor` and precedes `pageSetUpPr` in `CT_SheetPr`.
pub(super) fn set_outline_pr(xml: &str, now: OutlineSettings, was: OutlineSettings) -> String {
    let mut xml = xml.to_string();
    if now == was {
        return xml;
    }
    let flag = |b: bool| if b { "1" } else { "0" };
    let element = |prefix: &str| {
        format!(
            "<{prefix}outlinePr summaryBelow=\"{}\" summaryRight=\"{}\"/>",
            flag(now.summary_below),
            flag(now.summary_right)
        )
    };
    let Some((start, end)) = worksheet_child_span(&xml, "sheetPr") else {
        let block = format!("<sheetPr>{}</sheetPr>", element(""));
        return put_worksheet_child(&xml, "sheetPr", &block, None, false);
    };
    let prefix = prefix_at(&xml, start).to_string();
    let children = element_children(&xml[start..end]);
    match children.iter().find(|(n, _, _)| n == "outlinePr") {
        Some(&(_, cs, _)) => {
            let at = start + cs;
            xml = set_tag_attr(&xml, at, "summaryBelow", Some(flag(now.summary_below)));
            xml = set_tag_attr(&xml, at, "summaryRight", Some(flag(now.summary_right)));
        }
        None => {
            let after = children
                .iter()
                .filter(|(n, _, _)| n == "tabColor")
                .map(|&(_, _, ce)| start + ce)
                .max();
            let content = open_up(&mut xml, start);
            let at = after.unwrap_or(content);
            xml.insert_str(at, &element(&prefix));
        }
    }
    xml
}

/// Sync `<sheetFormatPr outlineLevelRow outlineLevelCol>` with the deepest
/// row and column outline levels. Excel sizes its outline gutter from these.
/// A level of 0 removes the attribute; an element is added only when there is
/// an outline, with the `defaultRowHeight` the schema requires.
pub(super) fn set_outline_levels(
    xml: &str,
    rows: u8,
    cols: u8,
    default_row_height: Option<f64>,
) -> String {
    let want = |n: u8| (n > 0).then(|| n.to_string());
    let Some((start, _)) = worksheet_child_span(xml, "sheetFormatPr") else {
        if rows == 0 && cols == 0 {
            return xml.to_string();
        }
        let mut block = format!(
            "<sheetFormatPr defaultRowHeight=\"{}\"",
            default_row_height.unwrap_or(15.0)
        );
        for (attr, n) in [("outlineLevelRow", rows), ("outlineLevelCol", cols)] {
            if let Some(v) = want(n) {
                block.push_str(&format!(" {attr}=\"{v}\""));
            }
        }
        block.push_str("/>");
        return put_worksheet_child(xml, "sheetFormatPr", &block, None, false);
    };
    let a = StartTag::read(&xml[start..]);
    let mut out = xml.to_string();
    for (attr, n) in [("outlineLevelRow", rows), ("outlineLevelCol", cols)] {
        let have = a.num::<u8>(attr).unwrap_or(0);
        if have != n {
            out = set_tag_attr(&out, start, attr, want(n).as_deref());
        }
    }
    out
}

#[cfg(test)]
mod tests;
