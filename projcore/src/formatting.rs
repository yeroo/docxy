//! The plan's formatting (#1532, what #1390 asked for): the chosen bar
//! palette, text styles per category (Project's Text Styles "Item to
//! Change") and per cell, the gridline and link-line styles. It is part of
//! the plan ([`crate::model::Project::formatting`]), edited as one undo step
//! through the editor, saved in the `.yppx` package's `formatting.xml` part
//! ([`crate::yppx`]) and drawn by the hosts' table and chart. MSPDI carries
//! none of it, as in Project. The dialogs that edit it may come from a UI
//! plugin (a command's `view` reply, [`Formatting::apply_view`]) or later from
//! the built-ins. The gridlines are Project's Gridlines dialog as data
//! ([`gridlines`], #1575).

use crate::json::Json;
use opccore::xml::{Event, XmlParser};

pub mod bars;
pub mod gridlines;
pub use bars::{BarRow, BarText, Facts, Mark, Middle, TaskBar};
pub use gridlines::{Gridline, Gridlines, LineType};

/// The `.yppx` part the formatting is saved in.
pub const PART: &str = "formatting.xml";
/// Its content type in `[Content_Types].xml`.
pub const CONTENT_TYPE: &str = "application/vnd.yppx.formatting+xml";

/// A font and colour choice; `None` leaves the built-in drawing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextStyle {
    pub font: Option<String>,
    /// Points.
    pub size: Option<f32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    /// `0xRRGGBB`.
    pub color: Option<u32>,
    pub background: Option<u32>,
    pub strikethrough: Option<bool>,
    /// A [`PATTERNS`] name over `background`; any other text is kept as
    /// typed and drawn solid.
    pub pattern: Option<String>,
}

/// Project's `PjBackgroundPattern`, in enum order.
pub const PATTERNS: [&str; 14] = [
    "hollow",
    "solid",
    "light-fill",
    "medium-fill",
    "dark-fill",
    "diagonal-left",
    "diagonal-right",
    "dark-diagonal-left",
    "dark-diagonal-right",
    "thin-vertical-stripe",
    "medium-vertical-stripe",
    "light-dither",
    "medium-dither",
    "dark-dither",
];

impl TextStyle {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// This style with `over`'s set fields on top.
    pub fn over(mut self, over: &TextStyle) -> TextStyle {
        if over.font.is_some() {
            self.font.clone_from(&over.font);
        }
        if over.size.is_some() {
            self.size = over.size;
        }
        if over.bold.is_some() {
            self.bold = over.bold;
        }
        if over.italic.is_some() {
            self.italic = over.italic;
        }
        if over.underline.is_some() {
            self.underline = over.underline;
        }
        if over.color.is_some() {
            self.color = over.color;
        }
        if over.background.is_some() {
            self.background = over.background;
        }
        if over.strikethrough.is_some() {
            self.strikethrough = over.strikethrough;
        }
        if over.pattern.is_some() {
            self.pattern.clone_from(&over.pattern);
        }
        self
    }

    /// From a JSON object `{font, size, bold, italic, underline,
    /// strikethrough, color, background, pattern}` (colours `"#RRGGBB"` or numbers); unknown keys ignored.
    pub fn from_json(v: &Json) -> TextStyle {
        TextStyle {
            font: v.get_str("font").map(str::to_string),
            size: v.num("size").map(|n| n as f32),
            bold: v.get("bold").and_then(Json::as_bool),
            italic: v.get("italic").and_then(Json::as_bool),
            underline: v.get("underline").and_then(Json::as_bool),
            color: parse_color(v.get("color")),
            background: parse_color(v.get("background")),
            strikethrough: v.get("strikethrough").and_then(Json::as_bool),
            pattern: v
                .get_str("pattern")
                .filter(|p| !p.is_empty())
                .map(str::to_string),
        }
    }

    pub fn to_json(&self) -> Json {
        let mut pairs = Vec::new();
        if let Some(f) = &self.font {
            pairs.push(("font", Json::str(f.clone())));
        }
        if let Some(s) = self.size {
            pairs.push(("size", Json::Num(f64::from(s))));
        }
        for (key, flag) in [
            ("bold", self.bold),
            ("italic", self.italic),
            ("underline", self.underline),
            ("strikethrough", self.strikethrough),
        ] {
            if let Some(b) = flag {
                pairs.push((key, Json::Bool(b)));
            }
        }
        if let Some(c) = self.color {
            pairs.push(("color", Json::str(hex(c))));
        }
        if let Some(c) = self.background {
            pairs.push(("background", Json::str(hex(c))));
        }
        if let Some(p) = &self.pattern {
            pairs.push(("pattern", Json::str(p.clone())));
        }
        Json::obj(pairs)
    }

    fn attrs(&self, out: &mut String) {
        if let Some(f) = &self.font {
            push_attr(out, "font", f);
        }
        if let Some(s) = self.size {
            push_attr(out, "size", &trim_num(s));
        }
        for (key, flag) in [
            ("bold", self.bold),
            ("italic", self.italic),
            ("underline", self.underline),
            ("strikethrough", self.strikethrough),
        ] {
            if let Some(b) = flag {
                push_attr(out, key, if b { "1" } else { "0" });
            }
        }
        if let Some(c) = self.color {
            push_attr(out, "color", &hex(c));
        }
        if let Some(c) = self.background {
            push_attr(out, "background", &hex(c));
        }
        if let Some(p) = &self.pattern {
            push_attr(out, "pattern", p);
        }
    }

    fn from_attrs(parser: &XmlParser) -> TextStyle {
        let attr = |name: &str| {
            let v = decode(parser.attr(name));
            (!v.is_empty()).then_some(v)
        };
        let flag = |name: &str| attr(name).map(|v| v == "1" || v == "true");
        TextStyle {
            font: attr("font"),
            size: attr("size").and_then(|s| s.parse().ok()),
            bold: flag("bold"),
            italic: flag("italic"),
            underline: flag("underline"),
            color: attr("color").and_then(|c| parse_hex(&c)),
            background: attr("background").and_then(|c| parse_hex(&c)),
            strikethrough: flag("strikethrough"),
            pattern: attr("pattern"),
        }
    }
}

/// A text style for one category (`All`, `Critical Tasks`, `Summary Tasks`,
/// `Column Titles`, ..., or a category a plugin names).
#[derive(Debug, Clone, PartialEq)]
pub struct CategoryStyle {
    pub category: String,
    pub style: TextStyle,
}

/// A text style for one cell: task `uid`, column by heading.
#[derive(Debug, Clone, PartialEq)]
pub struct CellStyle {
    pub uid: i32,
    pub column: String,
    pub style: TextStyle,
}

/// The plan's formatting; the default is the built-in drawing throughout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Formatting {
    /// The bar palette's id (a plugin's, from its manifest).
    pub palette: Option<String>,
    pub text_styles: Vec<CategoryStyle>,
    pub cell_styles: Vec<CellStyle>,
    /// The lines set away from their default (#1575); see [`gridlines`].
    pub gridlines: Gridlines,
    pub link_style: Option<String>,
    /// Named view switches a UI plugin owns (Project's per-view toggles
    /// such as Slack, Late Tasks or a Task Path kind), sorted by name
    /// (#1553). The hosts keep none of their own here: `critical`,
    /// `baseline` and `timeline` stay the view's.
    pub switches: Vec<(String, bool)>,
    /// The view's Bar Styles table (#1696); empty keeps the palette drawing.
    pub bars: Vec<BarRow>,
    /// Format Bar's per-task overrides (#1696); see [`bars`].
    pub task_bars: Vec<TaskBar>,
}

impl Formatting {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// A switch's state, when one is stored.
    pub fn switch(&self, name: &str) -> Option<bool> {
        self.switches
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, on)| *on)
    }

    /// Set a switch, or clear it with `None`; the list stays sorted.
    pub fn set_switch(&mut self, name: &str, on: Option<bool>) {
        self.switches.retain(|(n, _)| n != name);
        if let Some(on) = on {
            self.switches.push((name.to_string(), on));
            self.switches.sort();
        }
    }

    /// The switches as the view JSON carries them: `{"name": true, ...}`.
    pub fn switches_json(&self) -> Json {
        Json::Obj(
            self.switches
                .iter()
                .map(|(n, on)| (n.clone(), Json::Bool(*on)))
                .collect(),
        )
    }

    /// The style of `category`, when one is set.
    pub fn category(&self, category: &str) -> Option<&TextStyle> {
        self.text_styles
            .iter()
            .find(|c| c.category == category)
            .map(|c| &c.style)
    }

    /// The styles of `categories` (the general first, the specific last)
    /// merged, each overriding the fields it sets.
    pub fn style_for(&self, categories: &[&str]) -> TextStyle {
        categories
            .iter()
            .filter_map(|c| self.category(c))
            .fold(TextStyle::default(), |acc, s| acc.over(s))
    }

    /// A cell's own style.
    pub fn cell(&self, uid: i32, column: &str) -> Option<&TextStyle> {
        self.cell_styles
            .iter()
            .find(|c| c.uid == uid && c.column.eq_ignore_ascii_case(column))
            .map(|c| &c.style)
    }

    /// Set (or, with an empty style, clear) a category's style.
    pub fn set_category(&mut self, category: &str, style: TextStyle) {
        self.text_styles.retain(|c| c.category != category);
        if !style.is_empty() {
            self.text_styles.push(CategoryStyle {
                category: category.to_string(),
                style,
            });
        }
    }

    /// Set (or, with an empty style, clear) a cell's style.
    pub fn set_cell(&mut self, uid: i32, column: &str, style: TextStyle) {
        self.cell_styles
            .retain(|c| !(c.uid == uid && c.column.eq_ignore_ascii_case(column)));
        if !style.is_empty() {
            self.cell_styles.push(CellStyle {
                uid,
                column: column.to_string(),
                style,
            });
        }
    }

    /// Apply a UI plugin command's `view` reply: `palette` (a string, or
    /// null to clear), `text_styles` (an object of category to style, a
    /// null clearing one), `cells` (an array of `{uid, column, ...style}`,
    /// an empty style clearing one), `gridlines` (an object of line name to
    /// its drawing, or null for every default: [`Gridlines::apply`], #1575)
    /// and `link_style` (a string or null). Other keys are the host's (its
    /// view switches) and ignored here. `true` when something changed.
    pub fn apply_view(&mut self, view: &Json) -> Result<bool, String> {
        let before = self.clone();
        let text = |v: &Json, key: &str| -> Result<Option<Option<String>>, String> {
            match v.get(key) {
                None => Ok(None),
                Some(Json::Null) => Ok(Some(None)),
                Some(Json::Str(s)) => Ok(Some(Some(s.clone()))),
                Some(_) => Err(format!("view.{key} is not a string or null")),
            }
        };
        if let Some(p) = text(view, "palette")? {
            self.palette = p.filter(|p| !p.is_empty());
        }
        if let Some(g) = view.get("gridlines") {
            self.gridlines.apply(g, "view.gridlines")?;
        }
        if let Some(l) = text(view, "link_style")? {
            self.link_style = l;
        }
        self.apply_bars(view)?;
        match view.get("switches") {
            None | Some(Json::Null) => {}
            Some(Json::Obj(pairs)) => {
                for (name, on) in pairs {
                    match on {
                        Json::Bool(on) => self.set_switch(name, Some(*on)),
                        Json::Null => self.set_switch(name, None),
                        _ => return Err(format!("view.switches.{name} is not a boolean or null")),
                    }
                }
            }
            Some(_) => return Err("view.switches is not an object".into()),
        }
        match view.get("text_styles") {
            None | Some(Json::Null) => {}
            Some(Json::Obj(pairs)) => {
                for (category, style) in pairs {
                    let style = match style {
                        Json::Null => TextStyle::default(),
                        Json::Obj(_) => TextStyle::from_json(style),
                        _ => return Err(format!("view.text_styles.{category} is not an object")),
                    };
                    self.set_category(category, style);
                }
            }
            Some(_) => return Err("view.text_styles is not an object".into()),
        }
        for (n, cell) in view.list("cells").iter().enumerate() {
            let uid = cell
                .int("uid")
                .and_then(|u| i32::try_from(u).ok())
                .ok_or_else(|| format!("view.cells[{n}] has no uid"))?;
            let column = cell.text("column");
            if column.is_empty() {
                return Err(format!("view.cells[{n}] has no column"));
            }
            self.set_cell(uid, &column, TextStyle::from_json(cell));
        }
        Ok(*self != before)
    }

    /// The `formatting.xml` part.
    pub fn to_xml(&self) -> String {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<formatting",
        );
        if let Some(p) = &self.palette {
            push_attr(&mut out, "palette", p);
        }
        if let Some(l) = &self.link_style {
            push_attr(&mut out, "link-style", l);
        }
        out.push_str(">\n");
        for c in &self.text_styles {
            out.push_str("  <text");
            push_attr(&mut out, "category", &c.category);
            c.style.attrs(&mut out);
            out.push_str("/>\n");
        }
        for c in &self.cell_styles {
            out.push_str("  <cell");
            push_attr(&mut out, "uid", &c.uid.to_string());
            push_attr(&mut out, "column", &c.column);
            c.style.attrs(&mut out);
            out.push_str("/>\n");
        }
        for (name, on) in &self.switches {
            out.push_str("  <switch");
            push_attr(&mut out, "name", name);
            push_attr(&mut out, "on", if *on { "1" } else { "0" });
            out.push_str("/>\n");
        }
        self.bars_to_xml(&mut out);
        self.gridlines.to_xml(&mut out);
        out.push_str("</formatting>\n");
        out
    }

    /// Read a `formatting.xml` part. Unknown elements and attributes are
    /// ignored, so a later build can add some.
    pub fn from_xml(xml: &str) -> Result<Formatting, String> {
        let mut parser = XmlParser::new(xml);
        let mut out = Formatting::default();
        let mut depth = 0usize;
        let mut seen_root = false;
        loop {
            match parser.next() {
                Event::Start => {
                    let name = parser.name();
                    if depth == 0 {
                        if name != "formatting" {
                            return Err(format!("{PART}: the root is <{name}>, not <formatting>"));
                        }
                        seen_root = true;
                        let attr = |n: &str| {
                            let v = decode(parser.attr(n));
                            (!v.is_empty()).then_some(v)
                        };
                        out.palette = attr("palette");
                        // An older build's `gridlines` root attribute (an
                        // opaque word nothing drew) is ignored.
                        out.link_style = attr("link-style");
                    } else if depth == 1 {
                        match name {
                            "text" => {
                                let category = decode(parser.attr("category"));
                                if !category.is_empty() {
                                    out.set_category(&category, TextStyle::from_attrs(&parser));
                                }
                            }
                            "cell" => {
                                let uid = decode(parser.attr("uid")).parse::<i32>().ok();
                                let column = decode(parser.attr("column"));
                                if let Some(uid) = uid.filter(|_| !column.is_empty()) {
                                    out.set_cell(uid, &column, TextStyle::from_attrs(&parser));
                                }
                            }
                            "switch" => {
                                let name = decode(parser.attr("name"));
                                let on = match decode(parser.attr("on")).trim() {
                                    "1" | "true" => Some(true),
                                    "0" | "false" => Some(false),
                                    _ => None,
                                };
                                if let Some(on) = on.filter(|_| !name.is_empty()) {
                                    out.set_switch(&name, Some(on));
                                }
                            }
                            "bar" | "task-bar" => out.read_bar_xml(name, &parser),
                            "gridline" => out.gridlines.read_xml(&parser),
                            _ => {}
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

fn hex(c: u32) -> String {
    format!("#{c:06X}")
}

fn parse_hex(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    (h.len() == 6)
        .then(|| u32::from_str_radix(h, 16).ok())
        .flatten()
}

fn parse_color(v: Option<&Json>) -> Option<u32> {
    match v? {
        Json::Num(n) if *n >= 0.0 && *n <= 0xFFFFFF as f64 => Some(*n as u32),
        Json::Str(s) => parse_hex(s),
        _ => None,
    }
}

/// `size` without a trailing `.0`.
pub(crate) fn trim_num(n: f32) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

pub(crate) fn push_attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// An attribute's raw text with the XML entities decoded.
pub(crate) fn decode(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Formatting {
        let mut f = Formatting {
            palette: Some("fx.blue".into()),
            link_style: None,
            ..Formatting::default()
        };
        f.gridlines.set(
            gridlines::Line::GanttRows,
            Gridline {
                line_type: LineType::Dotted,
                color: Some(0xCC3333),
                interval: 2,
                interval_type: LineType::Solid,
                interval_color: None,
            },
        );
        f.gridlines
            .set(gridlines::Line::SheetRows, Gridline::default());
        f.set_switch("fx.slack", Some(true));
        f.set_switch("fx.late", Some(false));
        f.set_category(
            "Summary Tasks",
            TextStyle {
                font: Some("Segoe UI <\"&>".into()),
                size: Some(11.5),
                bold: Some(true),
                italic: Some(false),
                underline: None,
                color: Some(0x1F3763),
                ..TextStyle::default()
            },
        );
        f.set_category(
            "All",
            TextStyle {
                size: Some(10.0),
                ..TextStyle::default()
            },
        );
        f.set_cell(
            3,
            "Name",
            TextStyle {
                color: Some(0xFF0000),
                ..TextStyle::default()
            },
        );
        f
    }

    #[test]
    fn the_part_round_trips() {
        let f = sample();
        let xml = f.to_xml();
        assert!(xml.starts_with("<?xml"), "{xml}");
        assert!(xml.contains(r#"<formatting palette="fx.blue">"#), "{xml}");
        assert!(
            xml.contains(
                r##"<gridline line="Gantt Rows" type="dotted" color="#CC3333" interval="2" interval-type="solid"/>"##
            ),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<gridline line="Sheet Rows" type="none"/>"#),
            "{xml}"
        );
        assert!(xml.contains(r##"font="Segoe UI &lt;&quot;&amp;&gt;" size="11.5" bold="1" italic="0" color="#1F3763""##), "{xml}");
        assert!(xml.contains(r#"<text category="All" size="10"/>"#), "{xml}");
        assert!(
            xml.contains(r##"<cell uid="3" column="Name" color="#FF0000"/>"##),
            "{xml}"
        );
        assert!(xml.contains(r#"<switch name="fx.late" on="0"/>"#), "{xml}");
        assert!(xml.contains(r#"<switch name="fx.slack" on="1"/>"#), "{xml}");
        assert_eq!(Formatting::from_xml(&xml).unwrap(), f);
        assert_eq!(
            Formatting::from_xml("<formatting/>").unwrap(),
            Formatting::default()
        );
        // Unknown things are ignored; a wrong root or broken XML is refused.
        let later = r#"<formatting palette="p" zoom="2"><later/><text category="X" bold="1" glow="1"/></formatting>"#;
        let got = Formatting::from_xml(later).unwrap();
        assert_eq!(got.palette.as_deref(), Some("p"));
        assert_eq!(got.category("X").unwrap().bold, Some(true));
        assert!(Formatting::from_xml("<views/>").is_err());
        assert!(Formatting::from_xml("<formatting><text").is_err());
        assert!(Formatting::from_xml("").is_err());
    }

    #[test]
    fn strikethrough_and_pattern_round_trip_and_merge() {
        let both = TextStyle {
            strikethrough: Some(true),
            pattern: Some("dark-dither".into()),
            ..TextStyle::default()
        };
        assert!(!both.is_empty());
        assert!(
            !TextStyle {
                strikethrough: Some(false),
                ..TextStyle::default()
            }
            .is_empty()
        );
        let json = both.to_json();
        assert_eq!(
            json.get("strikethrough").and_then(Json::as_bool),
            Some(true)
        );
        assert_eq!(json.get_str("pattern"), Some("dark-dither"));
        assert_eq!(TextStyle::from_json(&json), both);
        // Wrong types are ignored, as for the other fields.
        let bad = Json::parse(r#"{"strikethrough":"yes","pattern":3}"#).unwrap();
        assert_eq!(TextStyle::from_json(&bad), TextStyle::default());
        // An unknown name is kept verbatim.
        let odd = TextStyle {
            pattern: Some("zig<\"zag>&".into()),
            strikethrough: Some(false),
            ..TextStyle::default()
        };
        assert_eq!(TextStyle::from_json(&odd.to_json()), odd);
        let mut f = Formatting::default();
        f.set_category("All", both.clone());
        f.set_cell(2, "Name", odd.clone());
        let xml = f.to_xml();
        assert!(
            xml.contains(r#"<text category="All" strikethrough="1" pattern="dark-dither"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"strikethrough="0" pattern="zig&lt;&quot;zag&gt;&amp;""#),
            "{xml}"
        );
        assert_eq!(Formatting::from_xml(&xml).unwrap(), f);
        // A file without them loads unchanged.
        let old =
            Formatting::from_xml(r#"<formatting><text category="All" bold="1"/></formatting>"#)
                .unwrap();
        assert_eq!(old.category("All").unwrap().pattern, None);
        assert_eq!(old.category("All").unwrap().strikethrough, None);
        // over(): a set field replaces, an unset one keeps.
        let base = TextStyle {
            strikethrough: Some(true),
            pattern: Some("hollow".into()),
            ..TextStyle::default()
        };
        let top = TextStyle {
            strikethrough: Some(false),
            ..TextStyle::default()
        };
        let merged = base.clone().over(&top);
        assert_eq!(merged.strikethrough, Some(false));
        assert_eq!(merged.pattern.as_deref(), Some("hollow"));
        assert_eq!(base.over(&both).pattern.as_deref(), Some("dark-dither"));
        assert_eq!(PATTERNS.len(), 14);
        assert_eq!(PATTERNS[0], "hollow");
        // A view reply carries and applies both keys, in categories and cells.
        let mut g = Formatting::default();
        let view = Json::parse(
            r#"{"text_styles":{"All":{"strikethrough":true,"pattern":"light-fill"}},
                "cells":[{"uid":4,"column":"Name","strikethrough":false,"pattern":"solid"}]}"#,
        )
        .unwrap();
        g.apply_view(&view).unwrap();
        assert_eq!(
            g.category("All").unwrap().pattern.as_deref(),
            Some("light-fill")
        );
        assert_eq!(g.cell(4, "Name").unwrap().strikethrough, Some(false));
    }

    #[test]
    fn styles_merge_general_to_specific() {
        let f = sample();
        let s = f.style_for(&["All", "Summary Tasks", "Missing"]);
        assert_eq!(s.size, Some(11.5));
        assert_eq!(s.bold, Some(true));
        assert_eq!(f.style_for(&["All"]).size, Some(10.0));
        assert!(f.style_for(&["Missing"]).is_empty());
        assert_eq!(f.cell(3, "name").unwrap().color, Some(0xFF0000));
        assert_eq!(f.cell(4, "Name"), None);
        let mut g = f.clone();
        g.set_category("All", TextStyle::default());
        assert_eq!(g.category("All"), None);
        g.set_cell(3, "Name", TextStyle::default());
        assert!(g.cell_styles.is_empty());
    }

    #[test]
    fn a_view_reply_edits_the_formatting() {
        let mut f = Formatting::default();
        let view = Json::parse(
            r##"{"palette":"fx.red","critical":true,
                 "text_styles":{"Critical Tasks":{"color":"#CC0000","bold":true},"All":{"size":9}},
                 "cells":[{"uid":2,"column":"Duration","italic":true}],
                 "gridlines":{"Gantt Rows":{"type":"dotted"}},"link_style":"rectilinear"}"##,
        )
        .unwrap();
        assert!(f.apply_view(&view).unwrap());
        assert_eq!(f.palette.as_deref(), Some("fx.red"));
        assert_eq!(
            f.gridlines.get(gridlines::Line::GanttRows).line_type,
            LineType::Dotted
        );
        assert_eq!(
            f.apply_view(&Json::parse(r#"{"gridlines":"solid"}"#).unwrap())
                .unwrap_err(),
            "view.gridlines is not an object or null"
        );
        assert_eq!(f.category("Critical Tasks").unwrap().color, Some(0xCC0000));
        assert_eq!(f.category("All").unwrap().size, Some(9.0));
        assert_eq!(f.cell(2, "Duration").unwrap().italic, Some(true));
        assert_eq!(f.link_style.as_deref(), Some("rectilinear"));
        // Switches: set, kept sorted, cleared by null, refused when not a
        // boolean; the host's own flags (`critical`) are not stored.
        assert!(
            f.apply_view(
                &Json::parse(r#"{"switches":{"fx.slack":true,"fx.late":false}}"#).unwrap()
            )
            .unwrap()
        );
        assert_eq!(f.switch("fx.slack"), Some(true));
        assert_eq!(f.switch("fx.late"), Some(false));
        assert_eq!(f.switch("critical"), None);
        assert_eq!(
            f.switches,
            [
                ("fx.late".to_string(), false),
                ("fx.slack".to_string(), true)
            ]
        );
        assert_eq!(
            f.switches_json().to_string(),
            r#"{"fx.late":false,"fx.slack":true}"#
        );
        assert!(
            f.apply_view(&Json::parse(r#"{"switches":{"fx.late":null}}"#).unwrap())
                .unwrap()
        );
        assert_eq!(f.switch("fx.late"), None);
        assert!(
            f.apply_view(&Json::parse(r#"{"switches":{"fx.late":"yes"}}"#).unwrap())
                .is_err()
        );
        assert!(
            f.apply_view(&Json::parse(r#"{"switches":[]}"#).unwrap())
                .is_err()
        );
        // Nulls clear; an unchanged view changes nothing.
        let clear =
            Json::parse(r#"{"palette":null,"text_styles":{"All":null},"gridlines":null}"#).unwrap();
        assert!(f.apply_view(&clear).unwrap());
        assert_eq!(f.palette, None);
        assert_eq!(f.category("All"), None);
        assert!(f.gridlines.is_empty());
        assert!(
            !f.apply_view(&Json::parse(r#"{"critical":false}"#).unwrap())
                .unwrap()
        );
        assert!(
            f.apply_view(&Json::parse(r#"{"palette":3}"#).unwrap())
                .is_err()
        );
        assert!(
            f.apply_view(&Json::parse(r#"{"cells":[{"column":"Name"}]}"#).unwrap())
                .is_err()
        );
        assert!(
            f.apply_view(&Json::parse(r#"{"text_styles":[]}"#).unwrap())
                .is_err()
        );
        // JSON round trip of a style.
        let s = TextStyle::from_json(
            &Json::parse(r##"{"font":"Arial","size":12,"underline":true,"background":"#FFFF00"}"##)
                .unwrap(),
        );
        assert_eq!(TextStyle::from_json(&s.to_json()), s);
    }

    #[test]
    fn entities_decode() {
        assert_eq!(
            decode("a &amp; b &lt;c&gt; &quot;d&quot; &#65;&#x42; &bogus; &"),
            "a & b <c> \"d\" AB &bogus; &"
        );
    }
}
