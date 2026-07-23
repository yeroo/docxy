//! Category color mapping: Graph `outlookCategory.color` presets → terminal
//! colors, and a name→color lookup over the master category list. Presentation
//! only — the master list itself lives in the store (`mailcore`).

use mailcore::graph::model::MasterCategory;
use ratatui::style::Color;

/// Maps a Graph category color (`"preset0"`…`"preset24"`, or `"none"`) to a
/// best-effort terminal color. Unknown / `"none"` → `Color::Gray`.
pub fn preset_color(preset: &str) -> Color {
    match preset {
        "preset0" => Color::Red,
        "preset1" => Color::LightRed,
        "preset2" => Color::Yellow,
        "preset3" => Color::LightYellow,
        "preset4" => Color::Green,
        "preset5" => Color::Cyan,
        "preset6" => Color::LightGreen,
        "preset7" => Color::Blue,
        "preset8" => Color::Magenta,
        "preset9" => Color::LightMagenta,
        "preset10" => Color::LightBlue,
        "preset11" => Color::LightCyan,
        "preset12" => Color::Gray,
        "preset13" => Color::DarkGray,
        "preset14" => Color::White,
        "preset15" => Color::Red,
        "preset16" => Color::Yellow,
        "preset17" => Color::LightRed,
        "preset18" => Color::LightYellow,
        "preset19" => Color::Green,
        "preset20" => Color::Cyan,
        "preset21" => Color::LightGreen,
        "preset22" => Color::Blue,
        "preset23" => Color::Magenta,
        "preset24" => Color::LightMagenta,
        _ => Color::Gray,
    }
}

/// The color for a category `name`, looked up in the master list; a name not in
/// the list (deleted, or shared-mailbox) falls back to `Color::Gray`.
pub fn color_for(master: &[MasterCategory], name: &str) -> Color {
    master
        .iter()
        .find(|c| c.display_name == name)
        .map(|c| preset_color(&c.color))
        .unwrap_or(Color::Gray)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_and_lookup() {
        assert_eq!(preset_color("preset0"), Color::Red);
        assert_eq!(preset_color("none"), Color::Gray);
        assert_eq!(preset_color("bogus"), Color::Gray);
        let master = vec![MasterCategory {
            display_name: "Work".into(),
            color: "preset4".into(),
        }];
        assert_eq!(color_for(&master, "Work"), Color::Green);
        assert_eq!(color_for(&master, "Missing"), Color::Gray); // fallback
    }
}
