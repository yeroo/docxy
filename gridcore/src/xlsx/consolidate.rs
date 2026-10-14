//! `<dataConsolidate>`: Data ▸ Consolidate's settings for the sheet as a
//! destination, read at load and written back only when they changed, so
//! an untouched element stays byte-for-byte.

use opccore::xml::{Event, XmlParser};

use super::{
    decode, esc_attr, local, put_worksheet_child, remove_worksheet_child, worksheet_child_span,
};
use crate::edit::{
    ConsolidateRef, ConsolidateSettings, consolidate_token, format_consolidate_ref,
    parse_consolidate_func, split_ref_text,
};
use crate::print::setup::xml_bool;
use crate::sheet::{cell_name, parse_range_name, quote_sheet_name};

/// The sheet's `<dataConsolidate>`, leniently: an unknown function reads
/// as Sum, a missing flag as off. A reference to another workbook
/// (`r:id`) is kept as its element, apart from the list; one by defined
/// name keeps the name.
pub(super) fn read(xml: &str) -> Option<ConsolidateSettings> {
    if !xml.contains("dataConsolidate") {
        return None;
    }
    let (start, end) = worksheet_child_span(xml, "dataConsolidate")?;
    let part = &xml[start..end];
    let mut p = XmlParser::new(part);
    let mut s = ConsolidateSettings::default();
    let flag = |p: &XmlParser, name: &str| xml_bool(p.attr(name)).unwrap_or(false);
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "dataConsolidate" => {
                    s.func =
                        parse_consolidate_func(&decode(p.attr("function"))).unwrap_or_default();
                    s.top_row = flag(&p, "topLabels");
                    s.left_col = if p.attrs().iter().any(|a| a.name == "leftLabels") {
                        flag(&p, "leftLabels")
                    } else {
                        flag(&p, "startLabels")
                    };
                    s.links = flag(&p, "link");
                }
                "dataRef" => {
                    if p.attrs().iter().any(|a| local(a.name) == "id") {
                        // Only an empty element can be written back as is.
                        let raw = &part[p.start_pos()..p.pos()];
                        if raw.ends_with("/>") {
                            s.external.push(raw.to_string());
                        }
                        continue;
                    }
                    let (sheet, range) = (decode(p.attr("sheet")), decode(p.attr("ref")));
                    let text = match (sheet.is_empty(), range.is_empty()) {
                        (false, false) => match parse_range_name(&range) {
                            Some(area) => format_consolidate_ref(&ConsolidateRef { sheet, area }),
                            None => format!("{}!{range}", quote_sheet_name(&sheet)),
                        },
                        (true, false) => range,
                        _ => decode(p.attr("name")),
                    };
                    if !text.is_empty() {
                        s.refs.push(text);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Some(s)
}

/// Write `now` where it differs from `was` (what the part held at load):
/// replace the element, insert it at its `CT_Worksheet` place, or drop it.
pub(super) fn write(
    xml: &str,
    now: Option<&ConsolidateSettings>,
    was: Option<&ConsolidateSettings>,
) -> String {
    if now == was {
        return xml.to_string();
    }
    let Some(s) = now else {
        return remove_worksheet_child(xml, "dataConsolidate");
    };
    let mut el = String::from("<dataConsolidate");
    if s.func != Default::default() {
        el.push_str(&format!(" function=\"{}\"", consolidate_token(s.func)));
    }
    for (on, name) in [
        (s.left_col, "leftLabels"),
        (s.top_row, "topLabels"),
        (s.links, "link"),
    ] {
        if on {
            el.push_str(&format!(" {name}=\"1\""));
        }
    }
    let count = s.refs.len() + s.external.len();
    if count == 0 {
        el.push_str("/>");
    } else {
        el.push_str(&format!("><dataRefs count=\"{count}\">"));
        for text in &s.refs {
            el.push_str(&data_ref(text));
        }
        // Other workbooks' references as loaded, after this workbook's.
        for raw in &s.external {
            el.push_str(raw);
        }
        el.push_str("</dataRefs></dataConsolidate>");
    }
    put_worksheet_child(xml, "dataConsolidate", &el, None, true)
}

/// One `<dataRef>`: `ref` and `sheet` for a range (a deleted one,
/// `Sheet!#REF!`, as `ref="#REF!"`), `name` otherwise.
fn data_ref(text: &str) -> String {
    // `#REF!` ends in the `!` a sheet is split at: take it off first.
    let t = text.trim();
    let deleted = t.len() >= 5
        && t.is_char_boundary(t.len() - 5)
        && t[t.len() - 5..].eq_ignore_ascii_case("#REF!");
    if deleted {
        let head = &t[..t.len() - 5];
        let sheet = match head.strip_suffix('!') {
            Some(h) => match split_ref_text(&format!("{h}!A1")) {
                Some((Some(s), _)) => format!(" sheet=\"{}\"", esc_attr(&s)),
                _ => return format!("<dataRef name=\"{}\"/>", esc_attr(t)),
            },
            None if head.trim().is_empty() || head.trim() == "=" => String::new(),
            None => return format!("<dataRef name=\"{}\"/>", esc_attr(t)),
        };
        return format!("<dataRef ref=\"#REF!\"{sheet}/>");
    }
    match split_ref_text(text) {
        Some((sheet, range)) => {
            let r = match parse_range_name(range) {
                Some((r1, c1, r2, c2)) if (r1, c1) == (r2, c2) => cell_name(r1, c1),
                Some((r1, c1, r2, c2)) => {
                    format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
                }
                None => return format!("<dataRef name=\"{}\"/>", esc_attr(text.trim())),
            };
            let sheet = sheet
                .map(|s| format!(" sheet=\"{}\"", esc_attr(&s)))
                .unwrap_or_default();
            format!("<dataRef ref=\"{r}\"{sheet}/>")
        }
        None => format!("<dataRef name=\"{}\"/>", esc_attr(text.trim())),
    }
}
