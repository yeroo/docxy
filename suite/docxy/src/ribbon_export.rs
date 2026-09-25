//! The docx ribbon, exported as data for the editable-HTML page.
//!
//! The browser page (`htmlbundle/web/`) draws the same ribbon this suite draws,
//! from a checked-in snapshot, `htmlbundle/web/ribbon-docx.json`. This module
//! builds that snapshot from the live definitions — [`crate::docxy_ribbon`],
//! [`crate::table_tab`], [`crate::QAT_ITEMS`], the Backstage rail and the
//! Fluent icons the suite renders — and its test fails when the checked-in
//! copy drifts, so a ribbon change here cannot silently skip the browser.
//!
//! Regenerate with:
//!
//! ```text
//! UPDATE_RIBBON_SNAPSHOT=1 cargo test --manifest-path suite/Cargo.toml ribbon_export
//! ```
//!
//! Commands carry their `act` (the `Act` variant name): the page maps acts,
//! not command ids, to engine operations, since several buttons share an act
//! and gallery items have no id.

use std::collections::BTreeMap;

use gpui::{AssetSource, Hsla, Rgba};
use gpui_component::theme::ThemeColor;
use serde_json::{Value, json};

use crate::rs;
use crate::{Act, Control};

/// Where the snapshot lives, relative to this crate.
const SNAPSHOT: &str = "../../htmlbundle/web/ribbon-docx.json";

/// How to regenerate the snapshot (also written into it).
const REGENERATE: &str =
    "UPDATE_RIBBON_SNAPSHOT=1 cargo test --manifest-path suite/Cargo.toml ribbon_export";

struct Export {
    icons: BTreeMap<String, String>,
    missing: Vec<String>,
}

impl Export {
    fn icon(&mut self, name: &str) {
        if self.icons.contains_key(name) {
            return;
        }
        let path = format!("icons/{name}.svg");
        match crate::DocxyAssets.load(&path) {
            Ok(Some(bytes)) => {
                let svg = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
                self.icons.insert(name.to_string(), svg.trim().to_string());
            }
            _ => self.missing.push(name.to_string()),
        }
    }

    fn cmd(&mut self, c: &rs::Cmd<Act>) -> Value {
        self.icon(c.icon.0);
        json!({
            "id": c.id,
            "label": c.label,
            "icon": c.icon.0,
            "act": act_name(c.act),
            "keyTip": c.key_tip,
            "tip": { "title": c.tip.title, "body": c.tip.body, "shortcut": c.tip.shortcut },
        })
    }

    fn cmds(&mut self, cs: &[rs::Cmd<Act>]) -> Value {
        Value::Array(cs.iter().map(|c| self.cmd(c)).collect())
    }

    fn control(&mut self, c: &Control<Act>) -> Value {
        match c {
            Control::Large(cmd) => json!({ "kind": "large", "cmd": self.cmd(cmd) }),
            Control::Toggle(cmd) => json!({ "kind": "toggle", "cmd": self.cmd(cmd) }),
            Control::Column(cmds) => json!({ "kind": "column", "cmds": self.cmds(cmds) }),
            Control::Split { primary, menu } => json!({
                "kind": "split",
                "primary": self.cmd(primary),
                "menu": self.cmds(menu),
            }),
            Control::Dropdown { cmd, items } => json!({
                "kind": "dropdown",
                "cmd": self.cmd(cmd),
                "items": self.cmds(items),
            }),
            Control::Gallery(g) => json!({
                "kind": "gallery",
                "id": g.id,
                "tip": { "title": g.tip.title, "body": g.tip.body, "shortcut": g.tip.shortcut },
                "items": g.items.iter().map(|i| json!({
                    "label": i.label,
                    "preview": i.preview,
                    "act": act_name(i.act),
                })).collect::<Vec<_>>(),
            }),
            Control::Rows(rows) => {
                let rows: Vec<Value> = rows
                    .iter()
                    .map(|row| {
                        Value::Array(
                            row.iter()
                                .map(|cell| match cell {
                                    rs::Cell::Btn(cmd) => {
                                        json!({ "kind": "btn", "cmd": self.cmd(cmd) })
                                    }
                                    rs::Cell::Combo { cmd, wide } => json!({
                                        "kind": "combo",
                                        "cmd": self.cmd(cmd),
                                        "wide": wide,
                                    }),
                                })
                                .collect(),
                        )
                    })
                    .collect();
                json!({ "kind": "rows", "rows": rows })
            }
            Control::Separator => json!({ "kind": "separator" }),
        }
    }

    fn groups(&mut self, groups: &[rs::Group<Act>]) -> Value {
        Value::Array(
            groups
                .iter()
                .map(|g| {
                    json!({
                        "title": g.title,
                        "priority": g.priority,
                        "launcher": g.launcher.map(act_name),
                        "items": g.items.iter().map(|c| self.control(c)).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }
}

fn act_name(act: Act) -> String {
    format!("{act:?}")
}

/// `#rrggbb`, or `#rrggbbaa` when translucent.
fn hex(c: Hsla) -> String {
    let c = Rgba::from(c);
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.a >= 0.999 {
        format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b), b(c.a))
    }
}

/// The gpui-component theme colours the suite's chrome reads, per mode.
fn theme_tokens(t: &ThemeColor) -> Value {
    json!({
        "background": hex(t.background),
        "foreground": hex(t.foreground),
        "mutedForeground": hex(t.muted_foreground),
        "border": hex(t.border),
        "secondary": hex(t.secondary),
        "sidebar": hex(t.sidebar),
        "tabActive": hex(t.tab_active),
        "selection": hex(t.selection),
        "titleBar": hex(t.title_bar),
        "titleBarBorder": hex(t.title_bar_border),
        "statusBar": hex(t.status_bar),
        "statusBarBorder": hex(t.status_bar_border),
        "popover": hex(t.popover),
        "popoverForeground": hex(t.popover_foreground),
    })
}

/// The whole snapshot, pretty-printed with a trailing newline.
pub(crate) fn docx_snapshot() -> Result<String, String> {
    let mut ex = Export {
        icons: BTreeMap::new(),
        missing: Vec::new(),
    };
    let ribbon = crate::docxy_ribbon();
    let names = crate::ribbon_tab_set(crate::Kind::Docx);
    let mut tabs = Vec::new();
    for (tab, name, key) in names {
        match tab {
            None => tabs.push(json!({ "name": name, "keyTip": key, "kind": "backstage" })),
            Some(t) => {
                let def = &ribbon.tabs[crate::ribbon_tab_index(*t, crate::Kind::Docx)];
                assert_eq!(def.name, *name, "tab set and ribbon disagree");
                tabs.push(json!({
                    "name": def.name,
                    "keyTip": def.key_tip,
                    "kind": "ribbon",
                    "groups": ex.groups(&def.groups),
                }));
            }
        }
    }
    let table = crate::table_tab();
    let contextual = vec![json!({
        "name": table.name,
        "keyTip": table.key_tip,
        "context": "table",
        "groups": ex.groups(&table.groups),
    })];
    let qat: Vec<Value> = crate::QAT_ITEMS
        .iter()
        .map(|q| {
            ex.icon(q.icon);
            json!({
                "id": q.id,
                "icon": q.icon,
                "label": q.label,
                "tip": q.tip,
                "action": match q.action {
                    crate::QatAction::Undo => "undo",
                    crate::QatAction::Redo => "redo",
                },
            })
        })
        .collect();
    let backstage: Vec<Value> = crate::backstage_rail_items(false)
        .map(|item| {
            json!({
                "id": item.id,
                "label": item.display,
                "action": match item.action {
                    crate::BackstageRailAction::Back => "back",
                    crate::BackstageRailAction::New => "new",
                    crate::BackstageRailAction::Open => "open",
                    crate::BackstageRailAction::Save => "save",
                    crate::BackstageRailAction::SaveAs => "saveAs",
                    crate::BackstageRailAction::Export => "export",
                    crate::BackstageRailAction::Close => "close",
                },
            })
        })
        .collect();
    if !ex.missing.is_empty() {
        ex.missing.sort();
        ex.missing.dedup();
        return Err(format!("icons with no SVG: {:?}", ex.missing));
    }
    let snapshot = json!({
        "_generated": format!(
            "From suite/docxy (docxy_ribbon, table_tab, QAT_ITEMS, BACKSTAGE_RAIL, icons, theme). \
             Do not edit; regenerate: {REGENERATE}"
        ),
        "format": "docx",
        "tabs": tabs,
        "contextual": contextual,
        "qat": qat,
        "backstage": backstage,
        "theme": {
            "brand": format!("#{:06x}", crate::BRAND),
            "onBrand": format!("#{:06x}", crate::FILE_FG),
            "light": theme_tokens(&ThemeColor::light()),
            "dark": theme_tokens(&ThemeColor::dark()),
            // Print Layout: white sheets with dark ink on a grey canvas.
            "canvas": { "light": "#9a9a9a", "dark": "#2b2b2b" },
            "pageInk": "#202020",
        },
        "icons": ex.icons,
    });
    let mut text = serde_json::to_string_pretty(&snapshot).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT)
    }

    /// The checked-in snapshot is exactly what the live definitions produce.
    #[test]
    fn ribbon_export_matches_the_checked_in_snapshot() {
        let fresh = docx_snapshot().unwrap();
        let path = snapshot_path();
        if std::env::var_os("UPDATE_RIBBON_SNAPSHOT").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            on_disk == fresh,
            "{} is stale: the suite's docx ribbon changed. Regenerate it with:\n  {REGENERATE}",
            path.display()
        );
    }

    #[test]
    fn every_command_and_gallery_item_names_an_act() {
        let v: Value = serde_json::from_str(&docx_snapshot().unwrap()).unwrap();
        let text = v.to_string();
        assert!(text.contains("\"act\":\"Bold\""));
        assert!(text.contains("\"act\":\"H1\""), "gallery items carry acts");
        assert!(
            text.contains("\"act\":\"RowAbove\""),
            "the Table tab is exported"
        );
        assert_eq!(v["contextual"][0]["context"], "table");
        assert_eq!(v["tabs"][0]["kind"], "backstage");
        assert!(v["icons"]["bold"].as_str().unwrap().starts_with("<svg"));
        assert!(
            v["theme"]["light"]["background"]
                .as_str()
                .unwrap()
                .starts_with('#')
        );
    }
}
