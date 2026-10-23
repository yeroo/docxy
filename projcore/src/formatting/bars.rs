//! The plan's bar styles (#1696): Project's Bar Styles table and Format Bar
//! override as data. A [`BarRow`] is one line of the per-view table (name,
//! the task categories it shows for, the sub-row 1 to 4, the From and To
//! date fields, a start / middle / end look and five text fields); a
//! [`TaskBar`] overrides those looks for one task. An empty table keeps the
//! palette drawing. The hosts evaluate the categories ([`Facts`]) and the
//! fields ([`crate::editor::Editor::bar_facts`] and `bar_date`/`bar_text`);
//! a UI plugin's command `view` reply edits both ([`Formatting::apply_bars`]).
//!
//! Every name is kept as typed: one outside its vocabulary is drawn as the
//! default (as `pattern` already is, #1664), and a wrongly typed JSON value
//! is ignored per field.

use super::{Formatting, decode, hex, parse_color, parse_hex, push_attr};
use crate::json::Json;
use opccore::xml::XmlParser;

/// Sub-rows a task row can hold.
pub const MAX_ROW: u8 = 4;

/// Mark shapes (`start` / `end`); `none` draws nothing.
pub const MARK_SHAPES: [&str; 10] = [
    "none",
    "square",
    "diamond",
    "circle",
    "triangle-up",
    "triangle-down",
    "arrow-left",
    "arrow-right",
    "arrow-up",
    "arrow-down",
];

/// How a mark is painted.
pub const MARK_KINDS: [&str; 3] = ["solid", "dashed", "framed"];

/// The middle bar's shapes: full height, or a medium (8 px) or narrow (4 px)
/// bar at the top, middle or bottom of the bar zone (a full bar's height).
pub const MIDDLE_SHAPES: [&str; 7] = [
    "full",
    "medium-top",
    "medium-middle",
    "medium-bottom",
    "narrow-top",
    "narrow-middle",
    "narrow-bottom",
];

/// A start or end mark; `None` is "not set" (a row's default look, or a
/// task's "not overridden").
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mark {
    pub shape: Option<String>,
    /// One of [`MARK_KINDS`].
    pub kind: Option<String>,
    pub color: Option<u32>,
}

/// The bar between the marks.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Middle {
    /// One of [`MIDDLE_SHAPES`].
    pub shape: Option<String>,
    /// A [`super::PATTERNS`] name.
    pub pattern: Option<String>,
    pub color: Option<u32>,
}

/// The task field shown in each position around the bar.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BarText {
    pub left: Option<String>,
    pub right: Option<String>,
    pub top: Option<String>,
    pub bottom: Option<String>,
    pub inside: Option<String>,
}

impl Mark {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// This mark with `over`'s set fields on top.
    pub fn over(mut self, over: &Mark) -> Mark {
        if over.shape.is_some() {
            self.shape.clone_from(&over.shape);
        }
        if over.kind.is_some() {
            self.kind.clone_from(&over.kind);
        }
        if over.color.is_some() {
            self.color = over.color;
        }
        self
    }

    fn from_json(v: Option<&Json>) -> Mark {
        let Some(v) = v else { return Mark::default() };
        Mark {
            shape: text_of(v, "shape"),
            kind: text_of(v, "type"),
            color: parse_color(v.get("color")),
        }
    }

    fn to_json(&self) -> Json {
        let mut pairs = Vec::new();
        push_text(&mut pairs, "shape", &self.shape);
        push_text(&mut pairs, "type", &self.kind);
        if let Some(c) = self.color {
            pairs.push(("color", Json::str(hex(c))));
        }
        Json::obj(pairs)
    }

    fn attrs(&self, out: &mut String, prefix: &str) {
        push_opt(out, &format!("{prefix}-shape"), &self.shape);
        push_opt(out, &format!("{prefix}-type"), &self.kind);
        if let Some(c) = self.color {
            push_attr(out, &format!("{prefix}-color"), &hex(c));
        }
    }

    fn from_attrs(parser: &XmlParser, prefix: &str) -> Mark {
        Mark {
            shape: attr_of(parser, &format!("{prefix}-shape")),
            kind: attr_of(parser, &format!("{prefix}-type")),
            color: attr_of(parser, &format!("{prefix}-color")).and_then(|c| parse_hex(&c)),
        }
    }
}

impl Middle {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// This middle with `over`'s set fields on top.
    pub fn over(mut self, over: &Middle) -> Middle {
        if over.shape.is_some() {
            self.shape.clone_from(&over.shape);
        }
        if over.pattern.is_some() {
            self.pattern.clone_from(&over.pattern);
        }
        if over.color.is_some() {
            self.color = over.color;
        }
        self
    }

    fn from_json(v: Option<&Json>) -> Middle {
        let Some(v) = v else {
            return Middle::default();
        };
        Middle {
            shape: text_of(v, "shape"),
            pattern: text_of(v, "pattern"),
            color: parse_color(v.get("color")),
        }
    }

    fn to_json(&self) -> Json {
        let mut pairs = Vec::new();
        push_text(&mut pairs, "shape", &self.shape);
        push_text(&mut pairs, "pattern", &self.pattern);
        if let Some(c) = self.color {
            pairs.push(("color", Json::str(hex(c))));
        }
        Json::obj(pairs)
    }

    fn attrs(&self, out: &mut String) {
        push_opt(out, "middle-shape", &self.shape);
        push_opt(out, "middle-pattern", &self.pattern);
        if let Some(c) = self.color {
            push_attr(out, "middle-color", &hex(c));
        }
    }

    fn from_attrs(parser: &XmlParser) -> Middle {
        Middle {
            shape: attr_of(parser, "middle-shape"),
            pattern: attr_of(parser, "middle-pattern"),
            color: attr_of(parser, "middle-color").and_then(|c| parse_hex(&c)),
        }
    }
}

impl BarText {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// This text with `over`'s set positions on top.
    pub fn over(mut self, over: &BarText) -> BarText {
        for (to, from) in [
            (&mut self.left, &over.left),
            (&mut self.right, &over.right),
            (&mut self.top, &over.top),
            (&mut self.bottom, &over.bottom),
            (&mut self.inside, &over.inside),
        ] {
            if from.is_some() {
                to.clone_from(from);
            }
        }
        self
    }

    /// The five positions with their names, in drawing order.
    pub fn positions(&self) -> [(&'static str, Option<&str>); 5] {
        [
            ("left", self.left.as_deref()),
            ("right", self.right.as_deref()),
            ("top", self.top.as_deref()),
            ("bottom", self.bottom.as_deref()),
            ("inside", self.inside.as_deref()),
        ]
    }

    /// Whether any position names `field` (case-insensitive).
    #[cfg(test)]
    pub fn names(&self, field: &str) -> bool {
        self.positions()
            .iter()
            .any(|(_, f)| f.is_some_and(|f| f.eq_ignore_ascii_case(field)))
    }

    fn from_json(v: Option<&Json>) -> BarText {
        let Some(v) = v else {
            return BarText::default();
        };
        BarText {
            left: text_of(v, "left"),
            right: text_of(v, "right"),
            top: text_of(v, "top"),
            bottom: text_of(v, "bottom"),
            inside: text_of(v, "inside"),
        }
    }

    fn to_json(&self) -> Json {
        let mut pairs = Vec::new();
        for (name, field) in self.positions() {
            if let Some(f) = field {
                pairs.push((name, Json::str(f)));
            }
        }
        Json::obj(pairs)
    }

    fn attrs(&self, out: &mut String) {
        for (name, field) in self.positions() {
            if let Some(f) = field {
                push_attr(out, &format!("text-{name}"), f);
            }
        }
    }

    fn from_attrs(parser: &XmlParser) -> BarText {
        BarText {
            left: attr_of(parser, "text-left"),
            right: attr_of(parser, "text-right"),
            top: attr_of(parser, "text-top"),
            bottom: attr_of(parser, "text-bottom"),
            inside: attr_of(parser, "text-inside"),
        }
    }
}

/// One line of a view's Bar Styles table.
#[derive(Debug, Clone, PartialEq)]
pub struct BarRow {
    pub name: String,
    /// The `Show For ... Tasks` entries, each a category, optionally with a
    /// leading `Not `; empty shows for every task.
    pub show_for: Vec<String>,
    /// 1 to [`MAX_ROW`].
    pub row: u8,
    /// The task date field the bar starts at; empty is `Start`.
    pub from: String,
    /// The task date field it ends at; empty is `Finish`.
    pub to: String,
    pub start: Mark,
    pub middle: Middle,
    pub end: Mark,
    pub text: BarText,
}

impl Default for BarRow {
    fn default() -> Self {
        BarRow {
            name: String::new(),
            show_for: Vec::new(),
            row: 1,
            from: String::new(),
            to: String::new(),
            start: Mark::default(),
            middle: Middle::default(),
            end: Mark::default(),
            text: BarText::default(),
        }
    }
}

/// A task's override of its bar rows (Format Bar); an unset part is not
/// overridden.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaskBar {
    pub uid: i32,
    pub start: Mark,
    pub middle: Middle,
    pub end: Mark,
    pub text: BarText,
}

impl TaskBar {
    /// A bar with nothing overridden is the task's style: `Reset`.
    pub fn is_empty(&self) -> bool {
        self.start.is_empty()
            && self.middle.is_empty()
            && self.end.is_empty()
            && self.text.is_empty()
    }
}

/// `row` as the table keeps it.
pub fn clamp_row(n: i64) -> u8 {
    n.clamp(1, i64::from(MAX_ROW)) as u8
}

/// What a task is, for `Show For ... Tasks`. The editor builds it
/// (`Editor::bar_facts`); a category the plan holds no data for
/// (`Marked`, the Task Path kinds) is never true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Facts {
    pub summary: bool,
    pub milestone: bool,
    pub critical: bool,
    pub active: bool,
    pub manual: bool,
    pub external: bool,
    /// Status reads `Late`.
    pub late: bool,
    /// Bit `n - 1` is `Flag<n>`.
    pub flags: u32,
}

impl Facts {
    /// Whether the task is in `category` (a name, case-insensitive; no `Not`).
    pub fn is(&self, category: &str) -> bool {
        let c = category
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        match c.as_str() {
            "normal" => !self.summary && !self.milestone,
            "milestone" => self.milestone,
            "summary" => self.summary,
            "critical" => self.critical,
            "noncritical" => !self.critical,
            "active" => self.active,
            "inactive" => !self.active,
            // Project's own words for the scheduling modes and external
            // tasks, beside the short forms.
            "manual" | "manually scheduled" => self.manual,
            "auto" | "automatically scheduled" => !self.manual,
            "external" | "external task" | "external tasks" => self.external,
            "late" => self.late,
            _ => c
                .strip_prefix("flag")
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| (1..=20).contains(n))
                .is_some_and(|n| self.flags & (1 << (n - 1)) != 0),
        }
    }
}

/// An entry's `Not ` prefix, split off (case-insensitive, any spacing).
fn split_not(entry: &str) -> (bool, &str) {
    let e = entry.trim();
    match e.get(..3) {
        Some(p) if p.eq_ignore_ascii_case("not") && e[3..].starts_with(char::is_whitespace) => {
            (true, e[3..].trim())
        }
        _ => (false, e),
    }
}

impl BarRow {
    /// Whether the row shows for a task: every listed category matches and
    /// no `Not` one does, as Project's comma list is a conjunction (its
    /// default Task row, `Normal, Active, Not Manually Scheduled`, is one).
    /// An empty list shows for every task; a category the plan holds no data
    /// for never matches, so a row listing one shows for no task.
    pub fn applies(&self, facts: &Facts) -> bool {
        self.show_for.iter().all(|entry| {
            let (not, name) = split_not(entry);
            name.is_empty() || facts.is(name) != not
        })
    }

    fn from_json(v: &Json) -> BarRow {
        let show_for = match v.get("show_for") {
            Some(Json::Arr(items)) => items
                .iter()
                .filter_map(|i| i.as_str())
                .flat_map(split_list)
                .collect(),
            Some(Json::Str(s)) => split_list(s),
            _ => Vec::new(),
        };
        BarRow {
            name: v.text("name"),
            show_for,
            row: v.int("row").map_or(1, clamp_row),
            from: v.text("from"),
            to: v.text("to"),
            start: Mark::from_json(v.get("start")),
            middle: Middle::from_json(v.get("middle")),
            end: Mark::from_json(v.get("end")),
            text: BarText::from_json(v.get("text")),
        }
    }

    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("name", Json::str(self.name.clone())),
            ("show_for", Json::strs(&self.show_for)),
            ("row", Json::Num(f64::from(self.row))),
            ("from", Json::str(self.from.clone())),
            ("to", Json::str(self.to.clone())),
            ("start", self.start.to_json()),
            ("middle", self.middle.to_json()),
            ("end", self.end.to_json()),
            ("text", self.text.to_json()),
        ])
    }

    fn to_xml(&self, out: &mut String) {
        out.push_str("  <bar");
        push_attr(out, "name", &self.name);
        if !self.show_for.is_empty() {
            push_attr(out, "show-for", &self.show_for.join(","));
        }
        push_attr(out, "row", &self.row.to_string());
        push_opt(out, "from", &non_empty(&self.from));
        push_opt(out, "to", &non_empty(&self.to));
        self.start.attrs(out, "start");
        self.middle.attrs(out);
        self.end.attrs(out, "end");
        self.text.attrs(out);
        out.push_str("/>\n");
    }

    fn from_xml(parser: &XmlParser) -> BarRow {
        BarRow {
            name: decode(parser.attr("name")),
            show_for: split_list(&decode(parser.attr("show-for"))),
            row: decode(parser.attr("row"))
                .trim()
                .parse()
                .map_or(1, clamp_row),
            from: decode(parser.attr("from")),
            to: decode(parser.attr("to")),
            start: Mark::from_attrs(parser, "start"),
            middle: Middle::from_attrs(parser),
            end: Mark::from_attrs(parser, "end"),
            text: BarText::from_attrs(parser),
        }
    }
}

impl TaskBar {
    fn from_json(uid: i32, v: &Json) -> TaskBar {
        TaskBar {
            uid,
            start: Mark::from_json(v.get("start")),
            middle: Middle::from_json(v.get("middle")),
            end: Mark::from_json(v.get("end")),
            text: BarText::from_json(v.get("text")),
        }
    }

    pub fn to_json(&self) -> Json {
        let mut pairs = vec![("uid", Json::Num(f64::from(self.uid)))];
        if !self.start.is_empty() {
            pairs.push(("start", self.start.to_json()));
        }
        if !self.middle.is_empty() {
            pairs.push(("middle", self.middle.to_json()));
        }
        if !self.end.is_empty() {
            pairs.push(("end", self.end.to_json()));
        }
        if !self.text.is_empty() {
            pairs.push(("text", self.text.to_json()));
        }
        Json::obj(pairs)
    }

    fn to_xml(&self, out: &mut String) {
        out.push_str("  <task-bar");
        push_attr(out, "uid", &self.uid.to_string());
        self.start.attrs(out, "start");
        self.middle.attrs(out);
        self.end.attrs(out, "end");
        self.text.attrs(out);
        out.push_str("/>\n");
    }

    fn from_xml(parser: &XmlParser) -> Option<TaskBar> {
        let uid = decode(parser.attr("uid")).trim().parse::<i32>().ok()?;
        Some(TaskBar {
            uid,
            start: Mark::from_attrs(parser, "start"),
            middle: Middle::from_attrs(parser),
            end: Mark::from_attrs(parser, "end"),
            text: BarText::from_attrs(parser),
        })
    }
}

impl Formatting {
    /// Replace the Bar Styles table; every row's `row` is clamped to
    /// 1..=[`MAX_ROW`].
    pub fn set_bars(&mut self, mut bars: Vec<BarRow>) {
        for bar in &mut bars {
            bar.row = clamp_row(i64::from(bar.row));
        }
        self.bars = bars;
    }

    /// Set a task's override, or (when it overrides nothing) remove it:
    /// Format Bar's `Reset`.
    /// An existing entry is replaced in place, so re-sending an unchanged
    /// override changes nothing.
    pub fn set_task_bar(&mut self, bar: TaskBar) {
        let at = self.task_bars.iter().position(|t| t.uid == bar.uid);
        match (at, bar.is_empty()) {
            (Some(i), true) => {
                self.task_bars.remove(i);
            }
            (Some(i), false) => self.task_bars[i] = bar,
            (None, false) => self.task_bars.push(bar),
            (None, true) => {}
        }
    }

    /// A task's override, when one is stored.
    pub fn task_bar(&self, uid: i32) -> Option<&TaskBar> {
        self.task_bars.iter().find(|t| t.uid == uid)
    }

    /// The rows that show for a task, in table order.
    pub fn bars_for(&self, facts: &Facts) -> Vec<&BarRow> {
        self.bars.iter().filter(|r| r.applies(facts)).collect()
    }

    /// The highest sub-row any row uses; 0 with no table. A view's task row
    /// is as many bands tall (at least one).
    pub fn max_bar_row(&self) -> u8 {
        self.bars.iter().map(|r| r.row).max().unwrap_or(0)
    }

    /// Whether a `Flag<n>` category is named, so a host reads the flags only
    /// when something asks for them.
    pub fn bars_use_flags(&self) -> bool {
        self.bars.iter().flat_map(|r| &r.show_for).any(|e| {
            let (_, name) = split_not(e);
            name.get(..4)
                .is_some_and(|p| p.eq_ignore_ascii_case("flag"))
        })
    }

    /// The `bars` and `task_bars` of a view reply (see
    /// [`Formatting::apply_view`]): `bars` is an array that replaces the
    /// table (null clears it, an absent key keeps it); `task_bars` is an
    /// array of `{uid, start?, middle?, end?, text?, reset?}` that sets each
    /// task's override (no parts, or `"reset": true`, removes it) and null
    /// clears every one.
    pub(super) fn apply_bars(&mut self, view: &Json) -> Result<(), String> {
        match view.get("bars") {
            None => {}
            Some(Json::Null) => self.bars.clear(),
            Some(Json::Arr(items)) => {
                let mut rows = Vec::new();
                for (n, item) in items.iter().enumerate() {
                    if !matches!(item, Json::Obj(_)) {
                        return Err(format!("view.bars[{n}] is not an object"));
                    }
                    rows.push(BarRow::from_json(item));
                }
                self.set_bars(rows);
            }
            Some(_) => return Err("view.bars is not an array or null".into()),
        }
        match view.get("task_bars") {
            None => {}
            Some(Json::Null) => self.task_bars.clear(),
            Some(Json::Arr(items)) => {
                let mut bars = Vec::new();
                for (n, item) in items.iter().enumerate() {
                    if !matches!(item, Json::Obj(_)) {
                        return Err(format!("view.task_bars[{n}] is not an object"));
                    }
                    let uid = item
                        .int("uid")
                        .and_then(|u| i32::try_from(u).ok())
                        .ok_or_else(|| format!("view.task_bars[{n}] has no uid"))?;
                    let mut bar = TaskBar::from_json(uid, item);
                    if item.flag("reset") {
                        bar = TaskBar {
                            uid,
                            ..TaskBar::default()
                        };
                    }
                    bars.push(bar);
                }
                for bar in bars {
                    self.set_task_bar(bar);
                }
            }
            Some(_) => return Err("view.task_bars is not an array or null".into()),
        }
        Ok(())
    }

    /// The table as the view JSON carries it.
    pub fn bars_json(&self) -> Json {
        Json::Arr(self.bars.iter().map(BarRow::to_json).collect())
    }

    /// The overrides as the view JSON carries them.
    pub fn task_bars_json(&self) -> Json {
        Json::Arr(self.task_bars.iter().map(TaskBar::to_json).collect())
    }

    pub(super) fn bars_to_xml(&self, out: &mut String) {
        for row in &self.bars {
            row.to_xml(out);
        }
        for bar in &self.task_bars {
            bar.to_xml(out);
        }
    }

    /// Read one `<bar>` or `<task-bar>` element the parser stands on.
    pub(super) fn read_bar_xml(&mut self, name: &str, parser: &XmlParser) {
        match name {
            "bar" => self.bars.push(BarRow::from_xml(parser)),
            _ => {
                if let Some(bar) = TaskBar::from_xml(parser) {
                    self.set_task_bar(bar);
                }
            }
        }
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect()
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

fn text_of(v: &Json, key: &str) -> Option<String> {
    v.get_str(key).filter(|s| !s.is_empty()).map(str::to_string)
}

fn push_text(pairs: &mut Vec<(&'static str, Json)>, key: &'static str, v: &Option<String>) {
    if let Some(s) = v {
        pairs.push((key, Json::str(s.clone())));
    }
}

fn push_opt(out: &mut String, name: &str, v: &Option<String>) {
    if let Some(s) = v {
        push_attr(out, name, s);
    }
}

fn attr_of(parser: &XmlParser, name: &str) -> Option<String> {
    let v = decode(parser.attr(name));
    (!v.is_empty()).then_some(v)
}

#[cfg(test)]
mod tests;
