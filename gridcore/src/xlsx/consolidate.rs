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
/// (`r:id`) is left out; one by defined name keeps the name.
pub(super) fn read(xml: &str) -> Option<ConsolidateSettings> {
    if !xml.contains("dataConsolidate") {
        return None;
    }
    let (start, end) = worksheet_child_span(xml, "dataConsolidate")?;
    let mut p = XmlParser::new(&xml[start..end]);
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
    if s.refs.is_empty() {
        el.push_str("/>");
    } else {
        el.push_str(&format!("><dataRefs count=\"{}\">", s.refs.len()));
        for text in &s.refs {
            el.push_str(&data_ref(text));
        }
        el.push_str("</dataRefs></dataConsolidate>");
    }
    put_worksheet_child(xml, "dataConsolidate", &el, None, true)
}

/// One `<dataRef>`: `ref` and `sheet` for a range, `name` otherwise.
fn data_ref(text: &str) -> String {
    match split_ref_text(text) {
        Some((sheet, range)) => match parse_range_name(range) {
            Some((r1, c1, r2, c2)) => {
                let r = if (r1, c1) == (r2, c2) {
                    cell_name(r1, c1)
                } else {
                    format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
                };
                let sheet = sheet
                    .map(|s| format!(" sheet=\"{}\"", esc_attr(&s)))
                    .unwrap_or_default();
                format!("<dataRef ref=\"{r}\"{sheet}/>")
            }
            None => format!("<dataRef name=\"{}\"/>", esc_attr(text.trim())),
        },
        None => format!("<dataRef name=\"{}\"/>", esc_attr(text.trim())),
    }
}
