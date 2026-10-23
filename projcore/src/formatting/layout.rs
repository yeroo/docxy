//! Project's Layout dialog, Bars and Other sections (#1695): bar height,
//! roll-up, round bars to whole days, bar splits, drawings and the bar-text
//! date format. Every value is optional; unset is the chart's built-in
//! drawing (a 14 px bar, no roll-up, whole days). The hosts draw bar height,
//! roll-up and round bars; `show_splits`, `show_drawings` and
//! `bar_text_date_format` are stored values until splits, drawings and the
//! `PjDateFormat` samples exist.

use super::{Formatting, decode, push_attr};
use crate::json::Json;
use opccore::xml::XmlParser;

/// Bar height limits, in points (Project's list is 6 to 24).
pub const MIN_BAR_HEIGHT: u8 = 6;
pub const MAX_BAR_HEIGHT: u8 = 24;

/// The Layout dialog's chart settings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    /// Points, clamped to [`MIN_BAR_HEIGHT`]..=[`MAX_BAR_HEIGHT`]. Unset
    /// draws the built-in 14 px bar, which is not 12 pt (16 px).
    pub bar_height: Option<u8>,
    pub rollup: Option<bool>,
    pub hide_rollup_expanded: Option<bool>,
    pub round_bars: Option<bool>,
    pub show_splits: Option<bool>,
    pub show_drawings: Option<bool>,
    /// A `PjDateFormat` name or sample text, kept as typed.
    pub bar_text_date_format: Option<String>,
}

/// The seven keys, in the order they are written.
const KEYS: [&str; 7] = [
    "bar_height",
    "rollup",
    "hide_rollup_expanded",
    "round_bars",
    "show_splits",
    "show_drawings",
    "bar_text_date_format",
];

/// `bar_height` points at 96/72 as pixels.
pub fn points_to_px(points: u8) -> f32 {
    f32::from(points) * 96. / 72.
}

impl Layout {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The bar height in pixels, when one is set.
    pub fn bar_height_px(&self) -> Option<f32> {
        self.bar_height.map(points_to_px)
    }

    /// Roll-up on?
    pub fn rolls_up(&self) -> bool {
        self.rollup == Some(true)
    }

    /// Round bars to whole days? On unless turned off.
    pub fn rounds_bars(&self) -> bool {
        self.round_bars != Some(false)
    }

    fn get(&self, key: &str) -> Json {
        let flag = |v: Option<bool>| v.map_or(Json::Null, Json::Bool);
        match key {
            "bar_height" => self
                .bar_height
                .map_or(Json::Null, |h| Json::Num(f64::from(h))),
            "rollup" => flag(self.rollup),
            "hide_rollup_expanded" => flag(self.hide_rollup_expanded),
            "round_bars" => flag(self.round_bars),
            "show_splits" => flag(self.show_splits),
            "show_drawings" => flag(self.show_drawings),
            _ => self
                .bar_text_date_format
                .clone()
                .map_or(Json::Null, Json::Str),
        }
    }

    /// All seven keys, null for unset: the shape [`Layout::apply`] reads.
    pub fn to_json(&self) -> Json {
        Json::Obj(KEYS.iter().map(|k| (k.to_string(), self.get(k))).collect())
    }

    fn set(&mut self, key: &str, v: &Json) -> Result<(), String> {
        let bad = |what: &str| format!("view.layout.{key} is not {what} or null");
        let flag = |v: &Json| match v {
            Json::Null => Ok(None),
            Json::Bool(b) => Ok(Some(*b)),
            _ => Err(bad("a boolean")),
        };
        match key {
            "bar_height" => {
                self.bar_height = match v {
                    Json::Null => None,
                    Json::Num(n) if n.is_finite() => Some(
                        n.round()
                            .clamp(f64::from(MIN_BAR_HEIGHT), f64::from(MAX_BAR_HEIGHT))
                            as u8,
                    ),
                    _ => return Err(bad("a number")),
                }
            }
            "rollup" => self.rollup = flag(v)?,
            "hide_rollup_expanded" => self.hide_rollup_expanded = flag(v)?,
            "round_bars" => self.round_bars = flag(v)?,
            "show_splits" => self.show_splits = flag(v)?,
            "show_drawings" => self.show_drawings = flag(v)?,
            _ => {
                self.bar_text_date_format = match v {
                    Json::Null => None,
                    Json::Str(s) => Some(s.clone()).filter(|s| !s.is_empty()),
                    _ => return Err(bad("a string")),
                }
            }
        }
        Ok(())
    }

    /// A view reply's `layout`: absent keeps, null clears every value, an
    /// object sets the keys it names (a null value clears that key, unknown
    /// keys are ignored). Every key is checked before any is applied, so an
    /// error leaves the layout as it was.
    pub fn apply(&mut self, v: Option<&Json>) -> Result<(), String> {
        match v {
            None => Ok(()),
            Some(Json::Null) => {
                *self = Layout::default();
                Ok(())
            }
            Some(Json::Obj(pairs)) => {
                let mut next = self.clone();
                for (key, value) in pairs {
                    if KEYS.contains(&key.as_str()) {
                        next.set(key, value)?;
                    }
                }
                *self = next;
                Ok(())
            }
            Some(_) => Err("view.layout is not an object or null".into()),
        }
    }

    /// The `<layout/>` element; nothing when empty.
    pub(super) fn to_xml(&self, out: &mut String) {
        if self.is_empty() {
            return;
        }
        out.push_str("  <layout");
        if let Some(h) = self.bar_height {
            push_attr(out, "bar-height", &h.to_string());
        }
        for (name, key) in [
            ("rollup", "rollup"),
            ("hide-rollup-expanded", "hide_rollup_expanded"),
            ("round-bars", "round_bars"),
            ("show-splits", "show_splits"),
            ("show-drawings", "show_drawings"),
        ] {
            if let Json::Bool(on) = self.get(key) {
                push_attr(out, name, if on { "1" } else { "0" });
            }
        }
        if let Some(f) = &self.bar_text_date_format {
            push_attr(out, "bar-text-date-format", f);
        }
        out.push_str("/>\n");
    }

    /// Read a `<layout/>` element the parser stands on; a bad value is
    /// ignored.
    pub(super) fn read_xml(&mut self, parser: &XmlParser) {
        let flag = |n: &str| match decode(parser.attr(n)).trim() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        };
        let mut layout = Layout {
            rollup: flag("rollup"),
            hide_rollup_expanded: flag("hide-rollup-expanded"),
            round_bars: flag("round-bars"),
            show_splits: flag("show-splits"),
            show_drawings: flag("show-drawings"),
            ..Layout::default()
        };
        if let Ok(h) = decode(parser.attr("bar-height")).trim().parse::<i64>() {
            layout.bar_height =
                Some(h.clamp(i64::from(MIN_BAR_HEIGHT), i64::from(MAX_BAR_HEIGHT)) as u8);
        }
        let f = decode(parser.attr("bar-text-date-format"));
        layout.bar_text_date_format = (!f.is_empty()).then_some(f);
        *self = layout;
    }
}

impl Formatting {
    /// The layout as the view JSON carries it (all seven keys).
    pub fn layout_json(&self) -> Json {
        self.layout.to_json()
    }
}

#[cfg(test)]
mod tests;
