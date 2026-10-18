//! The Zoom and Timescale dialogs (#1535), opened by View > Zoom and the chart
//! menu. Both apply through [`ProjectView`]'s zoom commands.
use super::*;
use crate::dialog::{Control, ControlKind, Dialog, DialogOwner, Reaction, Value, catalog};
use crate::page_setup::{choice, chosen, ok_cancel, text_of};

const ENTIRE: &str = "Entire project";
const SELECTED: &str = "Selected tasks";
const CUSTOM: &str = "Custom";

/// The least and most days the Custom field accepts across the chart.
const MIN_CUSTOM_DAYS: i32 = 1;
const MAX_CUSTOM_DAYS: i32 = 100_000;

/// The Zoom radio group's items: the levels, then the two fits and Custom.
fn zoom_items() -> Vec<&'static str> {
    let mut items: Vec<&str> = zoom::LEVELS.iter().map(|l| l.name).collect();
    items.extend([ENTIRE, SELECTED, CUSTOM]);
    items
}

/// The Zoom dialog for `v`'s chart: a level (or Custom, for a free width),
/// the span the chart shows, or the whole plan or the selected tasks.
pub(crate) fn zoom_dialog(v: &ProjectView) -> Dialog {
    let mut d = Dialog::message(catalog::ZOOM, "Zoom", String::new(), &[], DialogOwner::Zoom);
    d.text = None;
    let items = zoom_items();
    let custom = items.len() - 1;
    let on = if zoom::is_preset(v.day_w()) {
        zoom::nearest(v.day_w())
    } else {
        custom
    };
    let radio = choice(
        "zoom",
        "&Zoom to",
        ControlKind::Radio,
        &items,
        Some(on),
        None,
    );
    let mut days = Control::new(
        "custom",
        "&Custom (days across the chart):",
        ControlKind::Number,
        Value::Text(format!("{}", v.visible_days().round().max(1.) as i64)),
    );
    days.enabled = on == custom;
    d.controls = vec![radio, days];
    d.buttons = ok_cancel();
    d.react = Some(Reaction(react_zoom));
    d.mark_opened();
    d
}

/// The Custom field is live only while Custom is chosen.
fn react_zoom(d: &mut Dialog, i: usize, _before: &Value) {
    if d.controls[i].name != "zoom" {
        return;
    }
    let custom = zoom_items().len() - 1;
    let on = chosen(d, "zoom") == Some(custom);
    if let Some(c) = d.controls.iter_mut().find(|c| c.name == "custom") {
        c.enabled = on;
    }
}

/// OK on the Zoom dialog. A refused choice changes nothing.
pub(crate) fn apply_zoom(v: &mut ProjectView, d: &Dialog) -> Result<String, String> {
    let items = zoom_items();
    match chosen(d, "zoom") {
        Some(i) if i < zoom::LEVELS.len() => Ok(v.zoom_to_level(i)),
        Some(i) if items[i] == ENTIRE => Ok(v.zoom_entire()),
        Some(i) if items[i] == SELECTED => v.zoom_selected(),
        Some(_) => {
            let days = text_of(d, "custom")
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|n| (MIN_CUSTOM_DAYS..=MAX_CUSTOM_DAYS).contains(n))
                .ok_or_else(|| {
                    format!(
                        "Custom is a whole number of days from {MIN_CUSTOM_DAYS} to {MAX_CUSTOM_DAYS}"
                    )
                })?;
            Ok(v.zoom_to_days(days as f32))
        }
        None => Err("Choose how far to zoom".into()),
    }
}

/// The date the tier labels are shown for: a fixed Monday, so the examples
/// read the same whenever the dialog opens.
fn example_day() -> i64 {
    projcore::DateTime::parse_mspdi("2026-03-02T08:00:00").map_or(0, |d| d.day_number())
}

fn label_example(unit: zoom::Unit, day: i64) -> String {
    unit.labels(day).first().cloned().unwrap_or_default()
}

fn shown(level: &zoom::Level, today: i64) -> [String; 4] {
    [
        level.major.name().to_string(),
        label_example(level.major, level.major.start_at_or_before(today)),
        level.minor.name().to_string(),
        label_example(level.minor, level.minor.start_at_or_before(today)),
    ]
}

/// The Timescale dialog: the level to show, and the two tiers it draws. The
/// tiers' units, labels and count follow the level and are not editable in this
/// first cut.
pub(crate) fn timescale_dialog(v: &ProjectView) -> Dialog {
    let mut d = Dialog::message(
        catalog::TIMESCALE,
        "Timescale",
        String::new(),
        &[],
        DialogOwner::Timescale,
    );
    d.text = None;
    let level = zoom::nearest(v.day_w());
    let names: Vec<&str> = zoom::LEVELS.iter().map(|l| l.name).collect();
    let today = example_day();
    let [major, major_label, minor, minor_label] = shown(&zoom::LEVELS[level], today);
    d.controls = vec![
        choice(
            "level",
            "&Timescale:",
            ControlKind::Dropdown,
            &names,
            Some(level),
            None,
        ),
        info("major", 0, major),
        info("major-label", 1, major_label),
        info("minor", 2, minor),
        info("minor-label", 3, minor_label),
        info("count", 4, "1".into()),
    ];
    d.buttons = ok_cancel();
    d.react = Some(Reaction(react_timescale));
    d.mark_opened();
    d
}

/// The read-only rows under the level: what its tiers show. They are labels
/// (the text drawn is the whole row), so the catalogue lists no field for them.
const ROWS: [&str; 5] = [
    "Major tier units: ",
    "Major tier label: ",
    "Minor tier units: ",
    "Minor tier label: ",
    "Count: ",
];

fn info(name: &'static str, row: usize, text: String) -> Control {
    Control::new(
        name,
        "",
        ControlKind::Label,
        Value::Text(format!("{}{text}", ROWS[row])),
    )
}

fn react_timescale(d: &mut Dialog, i: usize, _before: &Value) {
    if d.controls[i].name != "level" {
        return;
    }
    let Some(level) = chosen(d, "level").and_then(|k| zoom::LEVELS.get(k)) else {
        return;
    };
    let rows = shown(level, example_day());
    for (row, (name, text)) in ["major", "major-label", "minor", "minor-label"]
        .into_iter()
        .zip(rows)
        .enumerate()
    {
        if let Some(c) = d.controls.iter_mut().find(|c| c.name == name) {
            c.value = Value::Text(format!("{}{text}", ROWS[row]));
        }
    }
}

/// OK on the Timescale dialog: choose the level.
pub(crate) fn apply_timescale(v: &mut ProjectView, d: &Dialog) -> Result<String, String> {
    chosen(d, "level")
        .map(|i| v.zoom_to_level(i))
        .ok_or_else(|| "Choose a timescale".into())
}
