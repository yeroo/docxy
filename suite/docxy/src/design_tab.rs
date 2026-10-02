//! The Word ribbon's Design tab (#651): the Page Background group's Page
//! Color, Watermark and Page Borders.
//!
//! Page Color and Watermark are menus built here (with Word's headings and
//! separators; the dropdowns' `items` carry the same commands for
//! `ribbon-read` and the harness). Their dialogs (More Colors, Fill Effects,
//! Custom Watermark) and Page Borders live in `design_dialogs`.
//!
//! What each writes, and what Ctrl+Z does with it:
//! - Page Color edits the package (`w:background` in the document part and
//!   `w:displayBackgroundShape` in the settings) and has no undo, as the
//!   Layout tab's settings edits have none.
//! - A watermark goes into every header the sections show. The header parts
//!   are package edits with no undo (as editing a header is); a header it has
//!   to create is referenced from its section through the body editor, which
//!   is one undo step. So undo after watermarking a document that had no
//!   header unlinks the new header (the watermark goes from those pages)
//!   while a header that already existed keeps its watermark: a known
//!   limitation of the split, the same as creating a header today.
//! - Page Borders rewrite `w:pgBorders` through the body editor's sections:
//!   one undo step.
use super::*;
use docxcore::page_bg::PageBackground;
use docxcore::watermark::TextWatermarkSpec;

/// A Design tab command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DesignAct {
    /// A Page Background menu button: open its menu.
    Menu(DesignMenu),
    /// A page colour, or No Color (`None`).
    PageColor(Option<u32>),
    MoreColors,
    FillEffects,
    /// A gallery watermark, by its index in [`PRESETS`].
    Watermark(usize),
    CustomWatermark,
    RemoveWatermark,
    /// Open Borders and Shading on its Page Border tab.
    PageBorders,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DesignMenu {
    PageColor,
    Watermark,
}

/// Page Color's palette, shared with every colour list on the tab's dialogs:
/// the Office theme's ten base colours, then Word's ten standard ones.
pub(crate) const THEME_COLORS: [(&str, &str, u32); 10] = [
    ("color-white", "White, Background 1", 0xFFFFFF),
    ("color-black", "Black, Text 1", 0x000000),
    ("color-bg2", "Gray, Background 2", 0xE7E6E6),
    ("color-text2", "Blue-Gray, Text 2", 0x44546A),
    ("color-accent1", "Blue, Accent 1", 0x4472C4),
    ("color-accent2", "Orange, Accent 2", 0xED7D31),
    ("color-accent3", "Gray, Accent 3", 0xA5A5A5),
    ("color-accent4", "Gold, Accent 4", 0xFFC000),
    ("color-accent5", "Blue, Accent 5", 0x5B9BD5),
    ("color-accent6", "Green, Accent 6", 0x70AD47),
];
pub(crate) const STANDARD_COLORS: [(&str, &str, u32); 10] = [
    ("color-darkred", "Dark Red", 0xC00000),
    ("color-red", "Red", 0xFF0000),
    ("color-orange", "Orange", 0xFFC000),
    ("color-yellow", "Yellow", 0xFFFF00),
    ("color-lightgreen", "Light Green", 0x92D050),
    ("color-green", "Green", 0x00B050),
    ("color-lightblue", "Light Blue", 0x00B0F0),
    ("color-blue", "Blue", 0x0070C0),
    ("color-darkblue", "Dark Blue", 0x002060),
    ("color-purple", "Purple", 0x7030A0),
];

/// Every palette colour, theme row first.
pub(crate) fn palette() -> impl Iterator<Item = (&'static str, &'static str, u32)> {
    THEME_COLORS.into_iter().chain(STANDARD_COLORS)
}

/// A colour's palette name, else its hex.
pub(crate) fn color_name(rgb: u32) -> String {
    palette()
        .find(|c| c.2 == rgb)
        .map_or_else(|| format!("#{rgb:06X}"), |c| c.1.to_string())
}

/// One gallery watermark.
pub(crate) struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub text: &'static str,
    pub diagonal: bool,
}

const fn preset(
    id: &'static str,
    label: &'static str,
    text: &'static str,
    diagonal: bool,
) -> Preset {
    Preset {
        id,
        label,
        text,
        diagonal,
    }
}

/// Word's watermark gallery: each text diagonal (1) and horizontal (2), under
/// its gallery heading.
pub(crate) const PRESETS: [Preset; 12] = [
    preset("wm-confidential-1", "CONFIDENTIAL 1", "CONFIDENTIAL", true),
    preset("wm-confidential-2", "CONFIDENTIAL 2", "CONFIDENTIAL", false),
    preset("wm-donotcopy-1", "DO NOT COPY 1", "DO NOT COPY", true),
    preset("wm-donotcopy-2", "DO NOT COPY 2", "DO NOT COPY", false),
    preset("wm-draft-1", "DRAFT 1", "DRAFT", true),
    preset("wm-draft-2", "DRAFT 2", "DRAFT", false),
    preset("wm-sample-1", "SAMPLE 1", "SAMPLE", true),
    preset("wm-sample-2", "SAMPLE 2", "SAMPLE", false),
    preset("wm-asap-1", "ASAP 1", "ASAP", true),
    preset("wm-asap-2", "ASAP 2", "ASAP", false),
    preset("wm-urgent-1", "URGENT 1", "URGENT", true),
    preset("wm-urgent-2", "URGENT 2", "URGENT", false),
];

/// The gallery's headings, before the preset at each index.
const PRESET_HEADINGS: [(usize, &str); 3] =
    [(0, "Confidential"), (4, "Disclaimers"), (8, "Urgent")];

/// One row of a Design menu.
enum Row {
    Item {
        id: &'static str,
        label: &'static str,
        act: DesignAct,
    },
    Heading(&'static str),
    Separator,
}

fn item(id: &'static str, label: &'static str, act: DesignAct) -> Row {
    Row::Item { id, label, act }
}

/// Every menu's rows, in Word's order.
fn menu_rows(menu: DesignMenu) -> Vec<Row> {
    use DesignAct as D;
    match menu {
        DesignMenu::PageColor => {
            let mut rows = vec![Row::Heading("Theme Colors")];
            rows.extend(
                THEME_COLORS
                    .iter()
                    .map(|&(id, label, rgb)| item(id, label, D::PageColor(Some(rgb)))),
            );
            rows.push(Row::Heading("Standard Colors"));
            rows.extend(
                STANDARD_COLORS
                    .iter()
                    .map(|&(id, label, rgb)| item(id, label, D::PageColor(Some(rgb)))),
            );
            rows.extend([
                Row::Separator,
                item("color-none", "No Color", D::PageColor(None)),
                item("color-more", "More Colors...", D::MoreColors),
                item("color-fill", "Fill Effects...", D::FillEffects),
            ]);
            rows
        }
        DesignMenu::Watermark => {
            let mut rows = Vec::new();
            for (i, p) in PRESETS.iter().enumerate() {
                if let Some(&(_, h)) = PRESET_HEADINGS.iter().find(|h| h.0 == i) {
                    rows.push(Row::Heading(h));
                }
                rows.push(item(p.id, p.label, D::Watermark(i)));
            }
            rows.extend([
                Row::Separator,
                item("wm-custom", "Custom Watermark...", D::CustomWatermark),
                item("wm-remove", "Remove Watermark", D::RemoveWatermark),
            ]);
            rows
        }
    }
}

/// The group's menu buttons: (menu, id, icon, label, KeyTip).
const MENUS: [(DesignMenu, &str, &str, &str, &str); 2] = [
    (
        DesignMenu::PageColor,
        "pagecolor",
        "page-color",
        "Page Color",
        "PC",
    ),
    (
        DesignMenu::Watermark,
        "watermark",
        "watermark",
        "Watermark",
        "PW",
    ),
];

/// The Design ribbon tab.
pub(crate) fn design_tab() -> rs::Tab<Act> {
    let mut controls: Vec<Control<Act>> = MENUS
        .iter()
        .map(|&(menu, id, icon, label, key)| Control::Dropdown {
            cmd: cmdt(id, icon, label, Act::Design(DesignAct::Menu(menu)), "").key(key),
            items: menu_rows(menu)
                .into_iter()
                .filter_map(|row| match row {
                    Row::Item { id, label, act } => {
                        Some(rs::cmd(id, icon, label, Act::Design(act)))
                    }
                    _ => None,
                })
                .collect(),
        })
        .collect();
    controls.push(Control::Large(
        cmdt(
            "pageborders",
            "page-borders",
            "Page Borders",
            Act::Design(DesignAct::PageBorders),
            "",
        )
        .key("PB"),
    ));
    rs::tab(
        "Design",
        "G",
        vec![rs::group("Page Background", 30, controls)],
    )
}

/// The menu a Page Background button opens, by the button's id.
pub(crate) fn menu_of(id: &str) -> Option<DesignMenu> {
    MENUS.iter().find(|m| m.1 == id).map(|m| m.0)
}

fn menu_id(menu: DesignMenu) -> &'static str {
    MENUS.iter().find(|m| m.0 == menu).map_or("", |m| m.1)
}

/// A Design menu's items, with the live checked states.
pub(crate) fn menu_items(menu: DesignMenu, checked: impl Fn(Act) -> bool) -> Vec<menu::MenuItem> {
    menu_rows(menu)
        .into_iter()
        .map(|row| match row {
            Row::Item { id, label, act } => menu::MenuItem::Item(
                menu::Entry::new(id, label, "", Act::Design(act), true)
                    .checked(checked(Act::Design(act))),
            ),
            Row::Heading(h) => menu::MenuItem::Heading(h.into()),
            Row::Separator => menu::MenuItem::Separator,
        })
        .collect()
}

/// Whether a Design command can run on the tab now: Page Borders writes
/// section properties, which a Markdown document does not keep.
pub(crate) fn design_enabled(tab: Option<&DocTab>, act: DesignAct) -> bool {
    match act {
        DesignAct::PageBorders => {
            tab.is_some_and(|t| matches!(t.surface, Surface::Doc(_)) && !t.markdown)
        }
        _ => true,
    }
}

/// The body editor and package of a .docx tab, or why a Page Color or
/// Watermark command has nothing to act on.
pub(crate) fn body(tab: &mut DocTab) -> Result<(&mut Editor, &mut Package), String> {
    match (&mut tab.surface, tab.pkg.as_mut()) {
        (Surface::Doc(ed), Some(pkg)) => Ok((ed, pkg)),
        (Surface::Doc(_), None) => {
            Err("Page background needs a saved .docx (not Markdown or a new document)".into())
        }
        _ => Err("Page background needs a document".into()),
    }
}

/// The text watermark the document shows now, read through the body
/// editor's sections (which may hold unsaved header references).
pub(crate) fn current_watermark(tab: &DocTab) -> Option<docxcore::package::TextWatermark> {
    let (Surface::Doc(ed), Some(pkg)) = (&tab.surface, tab.pkg.as_ref()) else {
        return None;
    };
    pkg.shown_text_watermarks(&ed.sections()).into_iter().next()
}

/// The page colour, for the page view and the check marks.
pub(crate) fn page_background(tab: &DocTab) -> Option<PageBackground> {
    tab.pkg.as_ref()?.page_background()
}

/// The colour Print Layout paints a page sheet: the page colour (a
/// gradient's first colour), else white.
pub(crate) fn page_sheet_color(tab: &DocTab) -> u32 {
    page_background(tab).map_or(0xFFFFFF, |b| b.color)
}

/// The text and dimmed-text colours Print Layout draws on a sheet of colour
/// `sheet`: dark ink on a light page, light ink on a dark one, as Word draws
/// automatic text.
pub(crate) fn page_ink(sheet: u32) -> (u32, u32) {
    let [r, g, b] = [16, 8, 0].map(|s| f32::from(((sheet >> s) & 0xFF) as u8) / 255.0);
    // Relative luminance (Rec. 709 weights on the gamma-encoded channels is
    // close enough to pick a side).
    if 0.2126 * r + 0.7152 * g + 0.0722 * b < 0.45 {
        (0xF2F2F2, 0xB0B0B0)
    } else {
        (0x202020, 0x808080)
    }
}

/// Whether a Design command's choice is the current one, for the check mark.
pub(crate) fn design_checked(tab: &DocTab, act: DesignAct) -> bool {
    match act {
        DesignAct::PageColor(want) => {
            let bg = page_background(tab);
            match want {
                None => tab.pkg.is_some() && bg.is_none(),
                Some(rgb) => bg.is_some_and(|b| b.color == rgb && b.gradient.is_none()),
            }
        }
        DesignAct::Watermark(i) => current_watermark(tab).is_some_and(|w| {
            let p = &PRESETS[i];
            w.text == p.text && ((w.rotation - 315.0).abs() < 0.5) == p.diagonal
        }),
        _ => false,
    }
}

/// Set or remove the text watermark in every header the document shows.
/// An open header or footer is closed first, so its editor cannot later
/// write the part back over the watermark. Whether anything changed.
pub(crate) fn set_watermark(
    tab: &mut DocTab,
    spec: Option<&TextWatermarkSpec>,
) -> Result<bool, String> {
    body(tab)?;
    exit_hf_tab(tab);
    let (ed, pkg) = body(tab)?;
    let mut sects = ed.sections();
    let before = sects.clone();
    let parts = pkg.set_text_watermark(spec, &mut sects);
    let raws: Vec<(usize, String)> = sects
        .into_iter()
        .enumerate()
        .filter(|(k, raw)| *raw != before[*k])
        .collect();
    let refs = ed.replace_sections(&raws);
    Ok(parts || refs)
}

/// Run a Design command that edits the document (Page Color's colours and
/// No Color, a gallery watermark, Remove Watermark) on a tab. The status line
/// says what happened; an error says why nothing did. The tab turns dirty
/// only when something changed.
pub(crate) fn design_apply(tab: &mut DocTab, act: DesignAct) -> Result<(), String> {
    let (changed, status) = match act {
        DesignAct::PageColor(rgb) => {
            let (_, pkg) = body(tab)?;
            let bg = rgb.map(|color| PageBackground {
                color,
                gradient: None,
            });
            let changed = pkg.set_page_background(bg.as_ref());
            let status = match rgb {
                Some(c) => format!("Page color: {}", color_name(c)),
                None => "Page color: No Color".into(),
            };
            (changed, status)
        }
        DesignAct::Watermark(i) => {
            let p = &PRESETS[i];
            let spec = TextWatermarkSpec::preset(p.text, p.diagonal);
            (
                set_watermark(tab, Some(&spec))?,
                format!("Watermark: {}", p.label),
            )
        }
        DesignAct::RemoveWatermark => (set_watermark(tab, None)?, "Watermark removed".into()),
        DesignAct::Menu(_)
        | DesignAct::MoreColors
        | DesignAct::FillEffects
        | DesignAct::CustomWatermark
        | DesignAct::PageBorders => return Ok(()),
    };
    tab.dirty |= changed;
    tab.status = status.into();
    Ok(())
}

impl Docxy {
    /// Dispatch a Design tab command.
    pub(crate) fn design_act(
        &mut self,
        act: DesignAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match act {
            DesignAct::Menu(menu) => {
                let id = menu_id(menu);
                // As the Layout tab's menus: the button's place once drawn,
                // else about where it sits.
                let at = split_menu_anchor(&self.probes.borrow(), id).unwrap_or_else(|| {
                    let n = MENUS.iter().position(|m| m.0 == menu).unwrap_or(0);
                    point(px(12. + 64. * n as f32), px(140.))
                });
                if let Err(e) = self.open_split_menu(id, at, cx) {
                    self.set_status(e);
                }
                return;
            }
            DesignAct::MoreColors => {
                self.open_design_dialog(crate::design_dialogs::more_colors_dialog)
            }
            DesignAct::FillEffects => {
                self.open_design_dialog(crate::design_dialogs::fill_effects_dialog)
            }
            DesignAct::CustomWatermark => {
                self.open_design_dialog(crate::design_dialogs::watermark_dialog)
            }
            DesignAct::PageBorders => {
                self.open_design_dialog(crate::design_dialogs::page_borders_dialog)
            }
            _ => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if let Err(e) = design_apply(tab, act) {
                        tab.status = e.into();
                    }
                }
            }
        }
        self.refocus(window, cx);
    }

    /// Open a Design dialog over the active document, or say why it cannot.
    fn open_design_dialog(
        &mut self,
        build: impl FnOnce(&DocTab) -> Result<crate::dialog::Dialog, String>,
    ) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        match build(tab) {
            Ok(d) => tab.dialogs.push(d),
            Err(e) => tab.status = e.into(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
