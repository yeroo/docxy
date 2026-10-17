//! The Project table's saved view state (#1533): the width of each column,
//! keyed by field name so a later column set keeps them. It is part of the
//! plan ([`crate::model::Project::table_view`]), edited as one undo step
//! ([`crate::editor::Editor::edit_table_view`]) and saved in the `.yppx`
//! package's `tableview.xml` part ([`crate::yppx`]); MSPDI carries none.
//! Sort and filter are view state on the editor and are not saved.

use crate::formatting::{decode, push_attr, trim_num};
use opccore::xml::{Event, XmlParser};

/// The `.yppx` part the table view is saved in.
pub const PART: &str = "tableview.xml";
/// Its content type in `[Content_Types].xml`.
pub const CONTENT_TYPE: &str = "application/vnd.yppx.tableview+xml";

/// Narrowest and widest a column may be, in px.
pub const MIN_WIDTH: f32 = 24.;
pub const MAX_WIDTH: f32 = 600.;

/// Column widths by field name, in px.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TableView {
    pub widths: Vec<(String, f32)>,
}

impl TableView {
    pub fn is_empty(&self) -> bool {
        self.widths.is_empty()
    }

    /// The saved width of column `name`, if one was set.
    pub fn width(&self, name: &str) -> Option<f32> {
        self.widths.iter().find(|(n, _)| n == name).map(|&(_, w)| w)
    }

    /// Set a column's width, clamped to [`MIN_WIDTH`]..=[`MAX_WIDTH`] and
    /// rounded to whole px.
    pub fn set_width(&mut self, name: &str, width: f32) {
        let width = width.round().clamp(MIN_WIDTH, MAX_WIDTH);
        match self.widths.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = width,
            None => self.widths.push((name.to_string(), width)),
        }
    }

    /// Forget a column's saved width (it draws at its default again).
    pub fn clear_width(&mut self, name: &str) {
        self.widths.retain(|(n, _)| n != name);
    }

    /// The `tableview.xml` part.
    pub fn to_xml(&self) -> String {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<tableview>\n",
        );
        for (name, width) in &self.widths {
            out.push_str("  <column");
            push_attr(&mut out, "field", name);
            push_attr(&mut out, "width", &trim_num(*width));
            out.push_str("/>\n");
        }
        out.push_str("</tableview>\n");
        out
    }

    /// Read a `tableview.xml` part. Unknown elements and attributes are
    /// ignored, a width outside the limits is clamped, and a column without a
    /// readable width is skipped.
    pub fn from_xml(xml: &str) -> Result<TableView, String> {
        let mut parser = XmlParser::new(xml);
        let mut out = TableView::default();
        let mut depth = 0usize;
        let mut seen_root = false;
        loop {
            match parser.next() {
                Event::Start => {
                    let name = parser.name();
                    if depth == 0 {
                        if name != "tableview" {
                            return Err(format!("{PART}: the root is <{name}>, not <tableview>"));
                        }
                        seen_root = true;
                    } else if depth == 1 && name == "column" {
                        let field = decode(parser.attr("field"));
                        let width = decode(parser.attr("width")).trim().parse::<f32>().ok();
                        if let Some(width) = width.filter(|w| w.is_finite() && !field.is_empty()) {
                            out.set_width(&field, width);
                        }
                    }
                    depth += 1;
                }
                Event::End => depth = depth.saturating_sub(1),
                Event::Text => {}
                Event::Eof => break,
            }
        }
        if parser.is_malformed() || !seen_root {
            return Err(format!("{PART}: not well-formed XML"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_round_trips_and_clamps() {
        let mut v = TableView::default();
        assert!(v.is_empty());
        v.set_width("Name", 301.4);
        v.set_width("Finish & co", 5.);
        v.set_width("Start", 9999.);
        assert_eq!(v.width("Name"), Some(301.));
        assert_eq!(v.width("Finish & co"), Some(MIN_WIDTH));
        assert_eq!(v.width("Start"), Some(MAX_WIDTH));
        assert_eq!(TableView::from_xml(&v.to_xml()).unwrap(), v);
        v.clear_width("Name");
        assert_eq!(v.width("Name"), None);
    }

    #[test]
    fn reads_later_builds_and_refuses_the_wrong_root() {
        let got = TableView::from_xml(
            "<tableview future=\"1\"><column field=\"ID\" width=\"60\" x=\"y\"/>\
             <column field=\"Name\" width=\"wide\"/><other/></tableview>",
        )
        .unwrap();
        assert_eq!(got.widths, vec![("ID".to_string(), 60.)]);
        assert!(TableView::from_xml("<formatting/>").is_err());
        assert!(TableView::from_xml("<tableview><column").is_err());
        assert!(TableView::from_xml("").is_err());
    }
}
