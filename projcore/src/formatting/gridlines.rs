//! The plan's gridlines (#1575): Project's Gridlines dialog as data. Each
//! line Project's "Line to change" list names has a normal type and colour
//! and, for the row and column lines, an interval that draws every Nth line
//! differently. The default of every line is what the hosts drew before the
//! model existed, so a plan without gridlines looks as it did; a plan stores
//! only the lines that differ from their default. A UI plugin command's
//! `view.gridlines` reply edits them ([`Gridlines::apply`]); the hosts' table
//! and chart draw from them.

use super::{hex, parse_color, parse_hex, push_attr};
use crate::json::Json;
use opccore::xml::XmlParser;

/// How a gridline is drawn. The names are Project's `Type` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineType {
    #[default]
    None,
    Solid,
    Dotted,
    ShortDashed,
    LongDashed,
}

impl LineType {
    pub const ALL: [LineType; 5] = [
        LineType::None,
        LineType::Solid,
        LineType::Dotted,
        LineType::ShortDashed,
        LineType::LongDashed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            LineType::None => "none",
            LineType::Solid => "solid",
            LineType::Dotted => "dotted",
            LineType::ShortDashed => "short-dashed",
            LineType::LongDashed => "long-dashed",
        }
    }

    /// A type by name (Project's words, case-insensitive; a blank is none).
    pub fn parse(s: &str) -> Option<LineType> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "" | "none" => LineType::None,
            "solid" => LineType::Solid,
            "dotted" => LineType::Dotted,
            "short-dashed" | "short dashed" | "short-dash" => LineType::ShortDashed,
            "long-dashed" | "long dashed" | "long-dash" => LineType::LongDashed,
            _ => return None,
        })
    }

    /// The dash and gap lengths in pixels a host draws this type with, or
    /// `None` for a continuous line. The pattern is shared so the hosts and
    /// their tests agree.
    pub fn dash(self) -> Option<(f32, f32)> {
        match self {
            LineType::None | LineType::Solid => None,
            LineType::Dotted => Some((1., 2.)),
            LineType::ShortDashed => Some((4., 3.)),
            LineType::LongDashed => Some((8., 4.)),
        }
    }

    /// The `(start, length)` of every mark along a line `len` long, so a
    /// painter draws the type as a run of quads; a continuous line is one.
    pub fn segments(self, len: f32) -> Vec<(f32, f32)> {
        if self == LineType::None || len <= 0. {
            return Vec::new();
        }
        match self.dash() {
            None => vec![(0., len)],
            Some((on, off)) => {
                let mut out = Vec::new();
                let mut t = 0.;
                while t < len {
                    out.push((t, on.min(len - t)));
                    t += on + off;
                }
                out
            }
        }
    }
}

/// A line of Project's "Line to change" list, in the list's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Line {
    GanttRows,
    BarRows,
    MiddleTierColumn,
    BottomTierColumn,
    CurrentDate,
    SheetRows,
    SheetColumns,
    TitleVertical,
    TitleHorizontal,
    PageBreaks,
    ProjectStart,
    ProjectFinish,
    StatusDate,
    TopTierColumn,
}

impl Line {
    pub const ALL: [Line; 14] = [
        Line::GanttRows,
        Line::BarRows,
        Line::MiddleTierColumn,
        Line::BottomTierColumn,
        Line::CurrentDate,
        Line::SheetRows,
        Line::SheetColumns,
        Line::TitleVertical,
        Line::TitleHorizontal,
        Line::PageBreaks,
        Line::ProjectStart,
        Line::ProjectFinish,
        Line::StatusDate,
        Line::TopTierColumn,
    ];

    /// The name Project's list shows.
    pub fn name(self) -> &'static str {
        match self {
            Line::GanttRows => "Gantt Rows",
            Line::BarRows => "Bar Rows",
            Line::MiddleTierColumn => "Middle Tier Column",
            Line::BottomTierColumn => "Bottom Tier Column",
            Line::CurrentDate => "Current Date",
            Line::SheetRows => "Sheet Rows",
            Line::SheetColumns => "Sheet Columns",
            Line::TitleVertical => "Title Vertical",
            Line::TitleHorizontal => "Title Horizontal",
            Line::PageBreaks => "Page Breaks",
            Line::ProjectStart => "Project Start",
            Line::ProjectFinish => "Project Finish",
            Line::StatusDate => "Status Date",
            Line::TopTierColumn => "Top Tier Column",
        }
    }

    /// A line by its name, case-insensitive.
    pub fn parse(s: &str) -> Option<Line> {
        let want = s.trim();
        Line::ALL
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(want))
    }

    /// Whether Project's "At interval" group applies: the row and column
    /// lines, which repeat; a date or title line is one line.
    pub fn has_interval(self) -> bool {
        matches!(
            self,
            Line::GanttRows
                | Line::BarRows
                | Line::SheetRows
                | Line::SheetColumns
                | Line::TopTierColumn
                | Line::MiddleTierColumn
                | Line::BottomTierColumn
        )
    }

    /// What the hosts drew before the model (#1575): the table's row and
    /// column rules, the chart's row rules and bottom-tier (day) lines and the
    /// timescale's cell borders, all solid in the host's own colour; nothing
    /// else.
    pub fn default_style(self) -> Gridline {
        match self {
            Line::GanttRows
            | Line::BottomTierColumn
            | Line::SheetRows
            | Line::SheetColumns
            | Line::TitleVertical
            | Line::TitleHorizontal => Gridline::solid(),
            _ => Gridline::default(),
        }
    }
}

/// One line's drawing: its normal type and colour (`None` is Project's
/// Automatic, the host's own colour), and every `interval`th line's type and
/// colour (an `interval` of 0 is Project's `None`: every line is normal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gridline {
    pub line_type: LineType,
    pub color: Option<u32>,
    pub interval: u8,
    pub interval_type: LineType,
    pub interval_color: Option<u32>,
}

/// The largest interval Project's Other box takes.
pub const MAX_INTERVAL: u8 = 99;

impl Gridline {
    pub fn solid() -> Gridline {
        Gridline {
            line_type: LineType::Solid,
            ..Gridline::default()
        }
    }

    /// Whether `index` (0-based: the first row or column is 0) is drawn as
    /// an interval line: Project counts every Nth line, so with an interval
    /// of 2 the second, fourth, ... are.
    pub fn at_interval(&self, index: usize) -> bool {
        self.interval >= 2 && (index + 1).is_multiple_of(usize::from(self.interval))
    }

    /// The type and colour `index` draws with.
    pub fn style_at(&self, index: usize) -> (LineType, Option<u32>) {
        if self.at_interval(index) {
            (self.interval_type, self.interval_color)
        } else {
            (self.line_type, self.color)
        }
    }

    /// Whether anything is drawn at all.
    pub fn draws(&self) -> bool {
        self.line_type != LineType::None
            || (self.interval >= 2 && self.interval_type != LineType::None)
    }

    pub fn to_json(&self) -> Json {
        let color = |c: Option<u32>| c.map_or(Json::Null, |c| Json::Str(hex(c)));
        Json::obj(vec![
            ("type", Json::str(self.line_type.name())),
            ("color", color(self.color)),
            ("interval", Json::Num(f64::from(self.interval))),
            ("interval_type", Json::str(self.interval_type.name())),
            ("interval_color", color(self.interval_color)),
        ])
    }

    /// This line changed by a JSON object's keys (`type`, `color`,
    /// `interval`, `interval_type`, `interval_color`); a key absent keeps
    /// the field, a `color` of null is Automatic. `at` names the line for
    /// the messages.
    pub fn updated(&self, j: &Json, at: &str) -> Result<Gridline, String> {
        let mut out = *self;
        let line_type = |key: &str| -> Result<Option<LineType>, String> {
            match j.get(key) {
                None => Ok(None),
                Some(Json::Null) => Ok(Some(LineType::None)),
                Some(Json::Str(s)) => LineType::parse(s)
                    .map(Some)
                    .ok_or_else(|| format!("{at}.{key}: '{s}' is not a line type")),
                Some(_) => Err(format!("{at}.{key} is not a string")),
            }
        };
        let color = |key: &str| -> Result<Option<Option<u32>>, String> {
            match j.get(key) {
                None => Ok(None),
                Some(Json::Null) => Ok(Some(None)),
                Some(v) => parse_color(Some(v))
                    .map(|c| Some(Some(c)))
                    .ok_or_else(|| format!("{at}.{key} is not a colour (#RRGGBB)")),
            }
        };
        if let Some(t) = line_type("type")? {
            out.line_type = t;
        }
        if let Some(c) = color("color")? {
            out.color = c;
        }
        match j.get("interval") {
            None => {}
            Some(Json::Null) => out.interval = 0,
            Some(Json::Num(n)) if *n >= 0. && *n <= f64::from(MAX_INTERVAL) && n.fract() == 0. => {
                // 1 is every line, which is no interval.
                out.interval = if *n < 2. { 0 } else { *n as u8 };
            }
            Some(_) => {
                return Err(format!(
                    "{at}.interval is not a whole number from 0 to {MAX_INTERVAL}"
                ));
            }
        }
        if let Some(t) = line_type("interval_type")? {
            out.interval_type = t;
        }
        if let Some(c) = color("interval_color")? {
            out.interval_color = c;
        }
        Ok(out)
    }

    fn attrs(&self, out: &mut String) {
        push_attr(out, "type", self.line_type.name());
        if let Some(c) = self.color {
            push_attr(out, "color", &hex(c));
        }
        if self.interval >= 2 {
            push_attr(out, "interval", &self.interval.to_string());
            push_attr(out, "interval-type", self.interval_type.name());
            if let Some(c) = self.interval_color {
                push_attr(out, "interval-color", &hex(c));
            }
        }
    }

    fn from_attrs(parser: &XmlParser) -> Gridline {
        let attr = |n: &str| super::decode(parser.attr(n));
        let interval = attr("interval").trim().parse::<u8>().unwrap_or(0);
        Gridline {
            line_type: LineType::parse(&attr("type")).unwrap_or_default(),
            color: parse_hex(&attr("color")),
            interval: if (2..=MAX_INTERVAL).contains(&interval) {
                interval
            } else {
                0
            },
            interval_type: LineType::parse(&attr("interval-type")).unwrap_or_default(),
            interval_color: parse_hex(&attr("interval-color")),
        }
    }
}

/// The plan's gridlines: the lines set away from their default, in list
/// order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Gridlines {
    set: Vec<(Line, Gridline)>,
}

impl Gridlines {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// How `line` is drawn: as set, else its default.
    pub fn get(&self, line: Line) -> Gridline {
        self.set
            .iter()
            .find(|(l, _)| *l == line)
            .map_or_else(|| line.default_style(), |(_, g)| *g)
    }

    /// Set a line; one equal to its default is not stored. An interval on a
    /// line that has none is dropped.
    pub fn set(&mut self, line: Line, mut style: Gridline) {
        if !line.has_interval() {
            style.interval = 0;
        }
        if style.interval < 2 {
            style.interval = 0;
            style.interval_type = LineType::None;
            style.interval_color = None;
        }
        self.set.retain(|(l, _)| *l != line);
        if style != line.default_style() {
            self.set.push((line, style));
            self.set.sort_by_key(|(l, _)| *l);
        }
    }

    /// Back to the default.
    pub fn clear(&mut self, line: Line) {
        self.set.retain(|(l, _)| *l != line);
    }

    /// The lines set away from their default.
    pub fn set_lines(&self) -> impl Iterator<Item = (Line, Gridline)> + '_ {
        self.set.iter().copied()
    }

    /// Every line as drawn, by name, in list order: what a plugin's dialog
    /// shows and the view JSON carries.
    pub fn to_json(&self) -> Json {
        Json::Obj(
            Line::ALL
                .into_iter()
                .map(|l| (l.name().to_string(), self.get(l).to_json()))
                .collect(),
        )
    }

    /// A `view.gridlines` reply: `null` puts every line back to its default;
    /// an object names lines (Project's names), each `null` (its default) or
    /// an object of the keys [`Gridline::updated`] reads, over the line as
    /// drawn now. `at` names the key for the messages.
    pub fn apply(&mut self, view: &Json, at: &str) -> Result<(), String> {
        match view {
            Json::Null => self.set.clear(),
            Json::Obj(pairs) => {
                for (name, value) in pairs {
                    let line = Line::parse(name)
                        .ok_or_else(|| format!("{at}: '{name}' is not a gridline"))?;
                    match value {
                        Json::Null => self.clear(line),
                        Json::Obj(_) => {
                            let style = self.get(line).updated(value, &format!("{at}.{name}"))?;
                            self.set(line, style);
                        }
                        _ => return Err(format!("{at}.{name} is not an object or null")),
                    }
                }
            }
            _ => return Err(format!("{at} is not an object or null")),
        }
        Ok(())
    }

    /// The `<gridline>` children of `formatting.xml`.
    pub(super) fn to_xml(&self, out: &mut String) {
        for (line, style) in &self.set {
            out.push_str("  <gridline");
            push_attr(out, "line", line.name());
            style.attrs(out);
            out.push_str("/>\n");
        }
    }

    /// Read one `<gridline>` element the parser stands on.
    pub(super) fn read_xml(&mut self, parser: &XmlParser) {
        let name = super::decode(parser.attr("line"));
        if let Some(line) = Line::parse(&name) {
            self.set(line, Gridline::from_attrs(parser));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_the_list_is_projects() {
        for l in Line::ALL {
            assert_eq!(Line::parse(l.name()), Some(l));
            assert_eq!(Line::parse(&l.name().to_ascii_uppercase()), Some(l));
        }
        assert_eq!(Line::ALL[0].name(), "Gantt Rows");
        assert_eq!(Line::ALL[9].name(), "Page Breaks");
        assert_eq!(Line::parse("Rows"), None);
        for t in LineType::ALL {
            assert_eq!(LineType::parse(t.name()), Some(t));
        }
        assert_eq!(LineType::parse(""), Some(LineType::None));
        assert_eq!(LineType::parse("short dashed"), Some(LineType::ShortDashed));
        assert_eq!(LineType::parse("wavy"), None);
    }

    #[test]
    fn the_defaults_are_what_the_hosts_drew() {
        let g = Gridlines::default();
        assert!(g.is_empty());
        for l in Line::ALL {
            let solid = matches!(
                l,
                Line::GanttRows
                    | Line::BottomTierColumn
                    | Line::SheetRows
                    | Line::SheetColumns
                    | Line::TitleVertical
                    | Line::TitleHorizontal
            );
            assert_eq!(g.get(l).line_type == LineType::Solid, solid, "{}", l.name());
            assert_eq!(g.get(l).color, None, "{} is automatic", l.name());
            assert_eq!(g.get(l).interval, 0);
        }
    }

    #[test]
    fn a_default_is_not_stored_and_an_interval_needs_a_repeating_line() {
        let mut g = Gridlines::default();
        g.set(Line::GanttRows, Gridline::solid());
        assert!(g.is_empty(), "the default again");
        g.set(
            Line::GanttRows,
            Gridline {
                line_type: LineType::Dotted,
                color: Some(0xCC3333),
                interval: 2,
                interval_type: LineType::Solid,
                interval_color: Some(0x3366CC),
            },
        );
        assert_eq!(g.set_lines().count(), 1);
        assert!(g.get(Line::GanttRows).at_interval(1));
        assert!(!g.get(Line::GanttRows).at_interval(0));
        assert_eq!(
            g.get(Line::GanttRows).style_at(3),
            (LineType::Solid, Some(0x3366CC))
        );
        assert_eq!(
            g.get(Line::GanttRows).style_at(2),
            (LineType::Dotted, Some(0xCC3333))
        );
        // A date line takes no interval.
        g.set(
            Line::CurrentDate,
            Gridline {
                line_type: LineType::Solid,
                interval: 3,
                interval_type: LineType::Dotted,
                ..Gridline::default()
            },
        );
        assert_eq!(g.get(Line::CurrentDate).interval, 0);
        assert_eq!(g.get(Line::CurrentDate).interval_type, LineType::None);
        // An interval below 2 is none, and drops its type and colour.
        g.set(
            Line::SheetRows,
            Gridline {
                line_type: LineType::Solid,
                interval: 1,
                interval_type: LineType::Dotted,
                interval_color: Some(1),
                ..Gridline::default()
            },
        );
        assert_eq!(g.get(Line::SheetRows), Gridline::solid());
        assert_eq!(g.set_lines().count(), 2);
        g.clear(Line::GanttRows);
        assert_eq!(g.set_lines().count(), 1);
        assert!(g.get(Line::CurrentDate).draws());
        assert!(!g.get(Line::StatusDate).draws());
    }

    #[test]
    fn a_view_reply_edits_lines_by_name() {
        let mut g = Gridlines::default();
        let view = Json::parse(
            r##"{"Gantt Rows":{"type":"dotted","color":"#CC3333","interval":2,"interval_type":"solid","interval_color":"#3366CC"},
                 "Sheet Rows":{"type":"none"},"Current Date":{"type":"solid","color":3355443}}"##,
        )
        .unwrap();
        g.apply(&view, "view.gridlines").unwrap();
        assert_eq!(g.get(Line::GanttRows).interval, 2);
        assert_eq!(g.get(Line::GanttRows).interval_color, Some(0x3366CC));
        assert_eq!(g.get(Line::SheetRows).line_type, LineType::None);
        assert_eq!(g.get(Line::CurrentDate).color, Some(3355443));
        // A partial object keeps the other fields; a null colour is Automatic.
        g.apply(
            &Json::parse(r#"{"Gantt Rows":{"color":null}}"#).unwrap(),
            "view.gridlines",
        )
        .unwrap();
        assert_eq!(g.get(Line::GanttRows).color, None);
        assert_eq!(g.get(Line::GanttRows).line_type, LineType::Dotted);
        // The JSON out carries every line as drawn; `set_lines` only the set.
        let all = g.to_json();
        assert_eq!(all.as_object().unwrap().len(), 14);
        assert_eq!(all.get("Bar Rows").unwrap().text("type"), "none");
        assert_eq!(all.get("Sheet Columns").unwrap().text("type"), "solid");
        assert_eq!(g.set_lines().count(), 3);
        // A null line is its default; a null reply clears all.
        g.apply(
            &Json::parse(r#"{"Sheet Rows":null}"#).unwrap(),
            "view.gridlines",
        )
        .unwrap();
        assert_eq!(g.get(Line::SheetRows), Gridline::solid());
        g.apply(&Json::Null, "view.gridlines").unwrap();
        assert!(g.is_empty());
        // Refusals name the key.
        let bad = |s: &str| Gridlines::default().apply(&Json::parse(s).unwrap(), "view.gridlines");
        assert_eq!(
            bad(r#"{"Rows":{}}"#).unwrap_err(),
            "view.gridlines: 'Rows' is not a gridline"
        );
        assert_eq!(
            bad(r#"{"Gantt Rows":{"type":"wavy"}}"#).unwrap_err(),
            "view.gridlines.Gantt Rows.type: 'wavy' is not a line type"
        );
        assert_eq!(
            bad(r#"{"Gantt Rows":{"interval":2.5}}"#).unwrap_err(),
            "view.gridlines.Gantt Rows.interval is not a whole number from 0 to 99"
        );
        assert_eq!(
            bad(r#"{"Gantt Rows":{"color":"red"}}"#).unwrap_err(),
            "view.gridlines.Gantt Rows.color is not a colour (#RRGGBB)"
        );
        assert_eq!(
            bad(r#"{"Gantt Rows":3}"#).unwrap_err(),
            "view.gridlines.Gantt Rows is not an object or null"
        );
        assert_eq!(
            bad(r#""dotted""#).unwrap_err(),
            "view.gridlines is not an object or null"
        );
    }

    #[test]
    fn segments_follow_the_type() {
        assert_eq!(LineType::Solid.segments(10.), vec![(0., 10.)]);
        assert!(LineType::None.segments(10.).is_empty());
        assert_eq!(
            LineType::Dotted.segments(7.),
            vec![(0., 1.), (3., 1.), (6., 1.)]
        );
        assert_eq!(
            LineType::ShortDashed.segments(10.),
            vec![(0., 4.), (7., 3.)]
        );
        assert_eq!(LineType::LongDashed.segments(8.), vec![(0., 8.)]);
        assert!(LineType::Solid.segments(0.).is_empty());
    }
}
