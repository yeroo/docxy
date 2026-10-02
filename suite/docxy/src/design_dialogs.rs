//! The Design tab's dialogs (#651): Page Color's More Colors and Fill
//! Effects (its Gradient tab; Texture, Pattern and Picture are not built),
//! Custom Watermark (a text watermark; not a picture), and Page Borders (the
//! Page Border tab of Borders and Shading, with its Options...).
//!
//! Their accept buttons come here ([`click`]) rather than to `apply_dialog`,
//! because a watermark acts on the whole tab: the package's headers, the
//! body's sections and the open header editor. Options... is a child whose
//! OK writes its margins back into Page Borders' own (hidden) controls
//! ([`border_options_write_back`]); Page Borders' OK applies them.
use super::*;
use crate::design_tab::{
    THEME_COLORS, body, color_name, current_watermark, page_background, palette, set_watermark,
};
use crate::dialog::{
    Button, ButtonRole, ChildDialog, Control, ControlKind, Dialog, DialogOwner, Reaction, Value,
};
use docxcore::page_bg::{Gradient, GradientStyle, PageBackground};
use docxcore::sect::{BorderSide, PageBorders, PgBorderDisplay, PgBorderOffset};
use docxcore::watermark::TextWatermarkSpec;

fn buttons(ok: &str, apply: bool) -> Vec<Button> {
    let mut b = vec![
        Button {
            default: true,
            ..Button::new(ok, ButtonRole::Accept)
        },
        Button::new("Cancel", ButtonRole::Cancel),
    ];
    if apply {
        b.push(Button::new("Apply", ButtonRole::Apply));
    }
    b
}

fn form(id: &'static str, title: &str, owner: DialogOwner, controls: Vec<Control>) -> Dialog {
    let mut d = Dialog::message(id, title, String::new(), &[], owner);
    d.text = None;
    d.controls = controls;
    d.buttons = buttons("OK", false);
    d.mark_opened();
    d
}

fn text(name: &'static str, label: &str, value: &str) -> Control {
    Control::new(name, label, ControlKind::Text, Value::Text(value.into()))
}

fn number(name: &'static str, label: &str, value: u32) -> Control {
    Control::new(
        name,
        label,
        ControlKind::Number,
        Value::Text(value.to_string()),
    )
}

fn check(name: &'static str, label: &str, on: bool) -> Control {
    Control::new(name, label, ControlKind::Checkbox, Value::Bool(on))
}

fn choice<S: AsRef<str>>(
    name: &'static str,
    label: &str,
    kind: ControlKind,
    items: &[S],
    at: usize,
) -> Control {
    let mut c = Control::new(name, label, kind, Value::Choice(Some(at)));
    c.items = items.iter().map(|s| s.as_ref().to_string()).collect();
    c
}

/// A control the dialog keeps for its owner: not drawn, not listed by
/// `dialog-read`, and not settable by a script.
fn hidden(name: &'static str, value: String) -> Control {
    let mut c = Control::new(name, name, ControlKind::Label, Value::Text(value));
    c.visible = false;
    c.page = Some(usize::MAX);
    c
}

fn get_text(d: &Dialog, name: &str) -> String {
    match d.value(name) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

fn get_bool(d: &Dialog, name: &str) -> bool {
    matches!(d.value(name), Some(Value::Bool(true)))
}

fn get_choice(d: &Dialog, name: &str) -> usize {
    match d.value(name) {
        Some(Value::Choice(Some(i))) => *i,
        _ => 0,
    }
}

fn index(d: &Dialog, name: &str) -> Option<usize> {
    d.controls.iter().position(|c| c.name == name)
}

fn set_enabled(d: &mut Dialog, name: &str, on: bool) {
    if let Some(i) = index(d, name) {
        d.controls[i].enabled = on;
    }
}

// ---- colours ----------------------------------------------------------------

const AUTOMATIC: &str = "Automatic";

/// A colour list: `lead` (Automatic, Silver, …), then the Page Color
/// palette, then the current colour as `#RRGGBB` when it is none of those.
/// The index of `current`.
fn color_control(name: &'static str, label: &str, lead: &[&str], current: Option<u32>) -> Control {
    let mut items: Vec<String> = lead.iter().map(|s| s.to_string()).collect();
    items.extend(palette().map(|c| c.1.to_string()));
    let at = match current {
        None => 0,
        Some(rgb) => match items.iter().position(|i| color_of(i) == Some(Some(rgb))) {
            Some(i) => i,
            None => {
                items.push(format!("#{rgb:06X}"));
                items.len() - 1
            }
        },
    };
    choice(name, label, ControlKind::Dropdown, &items, at)
}

/// The colour a list item names: `Some(None)` for Automatic.
fn color_of(item: &str) -> Option<Option<u32>> {
    if item == AUTOMATIC {
        return Some(None);
    }
    if item == "Silver" {
        return Some(Some(0xC0C0C0));
    }
    if let Some(c) = palette().find(|c| c.1 == item) {
        return Some(Some(c.2));
    }
    parse_hex(item).map(Some)
}

fn parse_hex(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    (h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(h, 16).ok())
        .flatten()
}

fn chosen_color(d: &Dialog, name: &str) -> Option<u32> {
    let i = index(d, name)?;
    let c = &d.controls[i];
    c.items
        .get(get_choice(d, name))
        .and_then(|item| color_of(item))
        .flatten()
}

// ---- Page Color -------------------------------------------------------------

/// More Colors: a colour as hex RRGGBB.
pub(crate) fn more_colors_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let current = page_background(tab).map_or(0xFFFFFF, |b| b.color);
    Ok(form(
        "more-colors",
        "Colors",
        DialogOwner::DesignMoreColors,
        vec![text("hex", "Hex:", &format!("{current:06X}"))],
    ))
}

const GRADIENT_COLORS: [&str; 2] = ["One color", "Two colors"];

/// Fill Effects, on its Gradient tab: one or two colours and a shading
/// style.
pub(crate) fn fill_effects_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let bg = page_background(tab);
    let color1 = bg.map_or(THEME_COLORS[4].2, |b| b.color);
    let grad = bg.and_then(|b| b.gradient);
    let two = grad.is_some_and(|g| g.color2 != 0xFFFFFF);
    let color2 = grad.map_or(0xFFFFFF, |g| g.color2);
    let style = grad.map_or(GradientStyle::Horizontal, |g| g.style);
    let styles: Vec<&str> = GradientStyle::ALL.iter().map(|s| s.label()).collect();
    let mut d = form(
        "fill-effects",
        "Fill Effects",
        DialogOwner::DesignFillEffects,
        vec![
            choice(
                "colors",
                "Colors:",
                ControlKind::Radio,
                &GRADIENT_COLORS,
                usize::from(two),
            ),
            color_control("color1", "Color 1:", &[], Some(color1)),
            color_control("color2", "Color 2:", &["White"], Some(color2)),
            choice(
                "style",
                "Shading styles:",
                ControlKind::Radio,
                &styles,
                GradientStyle::ALL
                    .iter()
                    .position(|s| *s == style)
                    .unwrap_or(0),
            ),
        ],
    );
    set_enabled(&mut d, "color2", two);
    d.react = Some(Reaction(fill_effects_react));
    d.mark_opened();
    Ok(d)
}

/// Color 2 is for Two colors only.
fn fill_effects_react(d: &mut Dialog, i: usize, _: &Value) {
    if d.controls[i].name == "colors" {
        let two = get_choice(d, "colors") == 1;
        set_enabled(d, "color2", two);
    }
}

// ---- Custom Watermark -------------------------------------------------------

const WATERMARK_KINDS: [&str; 2] = ["No watermark", "Text watermark"];
const LAYOUTS: [&str; 2] = ["Diagonal", "Horizontal"];
const WATERMARK_FIELDS: [&str; 6] = ["text", "font", "size", "color", "semi", "layout"];

/// Custom Watermark (Word's Printed Watermark) on the current watermark, or
/// Word's defaults.
pub(crate) fn watermark_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let cur = current_watermark(tab);
    let rgb = |(r, g, b): (u8, u8, u8)| (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
    let color = cur.as_ref().and_then(|w| w.fill).map_or(0xC0C0C0, rgb);
    let size = cur
        .as_ref()
        .and_then(|w| w.font_size_pt)
        .map_or_else(|| "Auto".to_string(), |s| format!("{s}"));
    let diagonal = cur
        .as_ref()
        .is_none_or(|w| (w.rotation - 315.0).abs() < 0.5);
    let mut d = form(
        "watermark",
        "Printed Watermark",
        DialogOwner::DesignWatermark,
        vec![
            choice(
                "kind",
                "Watermark:",
                ControlKind::Radio,
                &WATERMARK_KINDS,
                usize::from(cur.is_some()),
            ),
            text(
                "text",
                "Text:",
                cur.as_ref().map_or("ASAP", |w| w.text.as_str()),
            ),
            text("font", "Font:", "Calibri"),
            text("size", "Size:", &size),
            color_control("color", "Color:", &["Silver"], Some(color)),
            check("semi", "Semitransparent", true),
            choice(
                "layout",
                "Layout:",
                ControlKind::Radio,
                &LAYOUTS,
                usize::from(!diagonal),
            ),
        ],
    );
    d.buttons = buttons("OK", true);
    watermark_react(&mut d, 0, &Value::Bool(false));
    d.react = Some(Reaction(watermark_react));
    d.mark_opened();
    Ok(d)
}

/// The text fields are for Text watermark only.
fn watermark_react(d: &mut Dialog, _: usize, _: &Value) {
    let on = get_choice(d, "kind") == 1;
    for name in WATERMARK_FIELDS {
        set_enabled(d, name, on);
    }
}

/// The watermark Custom Watermark asks for: `None` for No watermark.
fn watermark_spec(d: &Dialog) -> Result<Option<TextWatermarkSpec>, String> {
    if get_choice(d, "kind") == 0 {
        return Ok(None);
    }
    let text = get_text(d, "text");
    if text.trim().is_empty() {
        return Err("Type the watermark's text".into());
    }
    let font = get_text(d, "font");
    let size = get_text(d, "size");
    let size_pt = if size.trim().eq_ignore_ascii_case("auto") || size.trim().is_empty() {
        None
    } else {
        let pt: f32 = size
            .trim()
            .parse()
            .ok()
            .filter(|v: &f32| v.is_finite() && (1.0..=1638.0).contains(v))
            .ok_or_else(|| format!("Size must be Auto or a number of points; got '{size}'"))?;
        Some(pt)
    };
    Ok(Some(TextWatermarkSpec {
        text,
        font: if font.trim().is_empty() {
            "Calibri".into()
        } else {
            font.trim().into()
        },
        size_pt,
        color: chosen_color(d, "color").unwrap_or(0xC0C0C0),
        semitransparent: get_bool(d, "semi"),
        diagonal: get_choice(d, "layout") == 0,
    }))
}

// ---- Page Borders -----------------------------------------------------------

const SETTINGS: [&str; 5] = ["None", "Box", "Shadow", "3-D", "Custom"];
const NONE: usize = 0;
const BOX: usize = 1;
const SHADOW: usize = 2;
const THREE_D: usize = 3;
const CUSTOM: usize = 4;

/// Border styles: (`w:val`, label).
const STYLES: [(&str, &str); 15] = [
    ("single", "Single"),
    ("dotted", "Dotted"),
    ("dashed", "Dashed"),
    ("dotDash", "Dot dash"),
    ("dotDotDash", "Dot dot dash"),
    ("double", "Double"),
    ("triple", "Triple"),
    ("thinThickSmallGap", "Thin-thick"),
    ("thickThinSmallGap", "Thick-thin"),
    ("wave", "Wave"),
    ("doubleWave", "Double wave"),
    ("threeDEmboss", "3-D emboss"),
    ("threeDEngrave", "3-D engrave"),
    ("outset", "Outset"),
    ("inset", "Inset"),
];

/// Widths: (`w:sz` in eighths of a point, label).
const WIDTHS: [(u32, &str); 9] = [
    (2, "1/4 pt"),
    (4, "1/2 pt"),
    (6, "3/4 pt"),
    (8, "1 pt"),
    (12, "1 1/2 pt"),
    (18, "2 1/4 pt"),
    (24, "3 pt"),
    (36, "4 1/2 pt"),
    (48, "6 pt"),
];

const APPLY_TO: [&str; 4] = [
    "Whole document",
    "This section",
    "This section - First page only",
    "This section - All except first page",
];

/// Top, left, bottom, right: `PageBorders::sides` order.
const SIDES: [(&str, &str); 4] = [
    ("top", "Top"),
    ("left", "Left"),
    ("bottom", "Bottom"),
    ("right", "Right"),
];
/// The hidden margin each side's Options... value is kept in.
const SIDE_SPACE: [&str; 4] = ["space-top", "space-left", "space-bottom", "space-right"];
const MEASURE_FROM: [&str; 2] = ["Text", "Edge of page"];

/// The body editor of a document tab whose sections Page Borders may
/// change: a Markdown document keeps none.
fn border_editor(tab: &mut DocTab) -> Result<&mut Editor, String> {
    if tab.markdown {
        return Err("Page borders need a .docx (not Markdown)".into());
    }
    match &mut tab.surface {
        Surface::Doc(ed) => Ok(ed),
        _ => Err("Page borders need a document".into()),
    }
}

/// Borders and Shading on its Page Border tab, on the caret section's page
/// borders.
pub(crate) fn page_borders_dialog(tab: &DocTab) -> Result<Dialog, String> {
    if tab.markdown {
        return Err("Page borders need a .docx (not Markdown)".into());
    }
    let Surface::Doc(ed) = &tab.surface else {
        return Err("Page borders need a document".into());
    };
    let pb = PageBorders::parse(&crate::page_setup::caret_raw(ed));
    let sides: Vec<&BorderSide> = pb
        .as_ref()
        .map(|p| p.sides.iter().flatten().collect())
        .unwrap_or_default();
    let first = sides.first().copied();
    let all_four = pb
        .as_ref()
        .is_some_and(|p| p.sides.iter().all(Option::is_some));
    let setting = match (&pb, first) {
        (None, _) | (_, None) => NONE,
        // The setting is the sides' look; Options... margins may differ.
        (Some(_), Some(s)) if all_four && sides.iter().all(|x| same_look(x, s)) => {
            if s.shadow {
                SHADOW
            } else if s.frame {
                THREE_D
            } else {
                BOX
            }
        }
        _ => CUSTOM,
    };
    let mut styles: Vec<String> = STYLES.iter().map(|s| s.1.to_string()).collect();
    let style_at = match first {
        None => 0,
        Some(s) => STYLES
            .iter()
            .position(|x| x.0 == s.style)
            .unwrap_or_else(|| {
                // An Art border, or a style the list lacks, shows as itself.
                styles.push(s.style.clone());
                styles.len() - 1
            }),
    };
    let width_at = first.map_or(1, |s| {
        WIDTHS
            .iter()
            .position(|w| w.0 >= s.sz)
            .unwrap_or(WIDTHS.len() - 1)
    });
    let apply_at = match pb.as_ref().map(|p| p.display) {
        Some(PgBorderDisplay::FirstPage) => 2,
        Some(PgBorderDisplay::NotFirstPage) => 3,
        _ => 0,
    };
    // Word's defaults: 24 pt from the edge of the page.
    let from_page = pb
        .as_ref()
        .is_none_or(|p| p.offset_from == PgBorderOffset::Page);
    let default_space = if from_page { 24 } else { 4 };
    let mut controls = vec![
        choice(
            "setting",
            "Setting:",
            ControlKind::Radio,
            &SETTINGS,
            setting,
        ),
        choice("style", "Style:", ControlKind::Dropdown, &styles, style_at),
        color_control("color", "Color:", &[AUTOMATIC], first.and_then(|s| s.color)),
        choice(
            "width",
            "Width:",
            ControlKind::Dropdown,
            &WIDTHS.map(|w| w.1),
            width_at,
        ),
    ];
    for (k, (name, label)) in SIDES.iter().enumerate() {
        let on = pb.as_ref().is_some_and(|p| p.sides[k].is_some());
        controls.push(check(name, label, on));
    }
    controls.push(choice(
        "apply",
        "Apply to:",
        ControlKind::Dropdown,
        &APPLY_TO,
        apply_at,
    ));
    for (k, name) in SIDE_SPACE.iter().enumerate() {
        let space = pb
            .as_ref()
            .and_then(|p| p.sides[k].as_ref())
            .map_or(default_space, |s| s.space);
        controls.push(hidden(name, space.to_string()));
    }
    controls.push(hidden("from", usize::from(from_page).to_string()));
    controls.push(hidden(
        "zorder",
        if pb.as_ref().is_some_and(|p| p.z_order_back) {
            "back".into()
        } else {
            String::new()
        },
    ));
    let mut d = form(
        "page-borders",
        "Borders and Shading",
        DialogOwner::DesignPageBorders,
        controls,
    );
    d.buttons.push(Button::new(
        "Options...",
        ButtonRole::Open(ChildDialog::PageBorderOptions),
    ));
    sides_enabled(&mut d);
    d.react = Some(Reaction(page_borders_react));
    d.mark_opened();
    Ok(d)
}

/// Whether two sides look alike: all but their margin (`w:space`).
fn same_look(a: &BorderSide, b: &BorderSide) -> bool {
    BorderSide {
        space: b.space,
        ..a.clone()
    } == *b
}

/// The side boxes are for Custom; Box, Shadow and 3-D tick all four, None
/// clears them.
fn page_borders_react(d: &mut Dialog, i: usize, _: &Value) {
    if d.controls[i].name != "setting" {
        return;
    }
    let setting = get_choice(d, "setting");
    if setting != CUSTOM {
        for (name, _) in SIDES {
            if let Some(k) = index(d, name) {
                d.controls[k].value = Value::Bool(setting != NONE);
            }
        }
    }
    sides_enabled(d);
}

fn sides_enabled(d: &mut Dialog) {
    let custom = get_choice(d, "setting") == CUSTOM;
    for (name, _) in SIDES {
        set_enabled(d, name, custom);
    }
}

/// Border and Shading Options, on the margins Page Borders holds.
pub(crate) fn border_options_dialog(parent: &Dialog) -> Dialog {
    let mut controls: Vec<Control> = SIDES
        .iter()
        .zip(SIDE_SPACE)
        .map(|(&(name, label), space)| {
            let v = get_text(parent, space).parse().unwrap_or(24);
            number(name, &format!("{label}:"), v)
        })
        .collect();
    controls.push(choice(
        "from",
        "Measure from:",
        ControlKind::Dropdown,
        &MEASURE_FROM,
        get_text(parent, "from").parse().unwrap_or(1),
    ));
    form(
        "page-border-options",
        "Border and Shading Options",
        DialogOwner::DesignBorderOptions,
        controls,
    )
}

/// Options...' OK: its margins and Measure from go into Page Borders, which
/// applies them with its own OK.
pub(crate) fn border_options_write_back(child: &Dialog, parent: &mut Dialog) {
    let mut set = |name: &str, v: String| {
        if let Some(i) = index(parent, name) {
            parent.controls[i].value = Value::Text(v);
        }
    };
    for ((name, _), space) in SIDES.iter().zip(SIDE_SPACE) {
        set(space, get_text(child, name).trim().to_string());
    }
    set("from", get_choice(child, "from").to_string());
}

/// The page borders Page Borders asks for (`None` for Setting: None), and
/// the sections it applies to.
fn page_borders_of(d: &Dialog, ed: &Editor) -> Result<(Option<PageBorders>, Vec<usize>), String> {
    let setting = get_choice(d, "setting");
    let style_i = get_choice(d, "style");
    let style = STYLES.get(style_i).map_or_else(
        || {
            index(d, "style")
                .and_then(|i| d.controls[i].items.get(style_i).cloned())
                .unwrap_or_else(|| "single".into())
        },
        |s| s.0.to_string(),
    );
    let sz = WIDTHS[get_choice(d, "width").min(WIDTHS.len() - 1)].0;
    let color = chosen_color(d, "color");
    let mut sides: [Option<BorderSide>; 4] = Default::default();
    for (k, (name, label)) in SIDES.iter().enumerate() {
        let on = match setting {
            NONE => false,
            CUSTOM => get_bool(d, name),
            _ => true,
        };
        if !on {
            continue;
        }
        let raw = get_text(d, SIDE_SPACE[k]);
        let space: u32 = raw
            .trim()
            .parse()
            .ok()
            .filter(|v| *v <= 31)
            .ok_or_else(|| format!("{label} margin must be 0 to 31 pt; got '{raw}'"))?;
        sides[k] = Some(BorderSide {
            style: style.clone(),
            sz,
            space,
            color,
            shadow: setting == SHADOW,
            frame: setting == THREE_D,
        });
    }
    let apply = get_choice(d, "apply");
    let pb = sides.iter().any(Option::is_some).then(|| PageBorders {
        sides,
        display: match apply {
            2 => PgBorderDisplay::FirstPage,
            3 => PgBorderDisplay::NotFirstPage,
            _ => PgBorderDisplay::AllPages,
        },
        offset_from: if get_text(d, "from") == "0" {
            PgBorderOffset::Text
        } else {
            PgBorderOffset::Page
        },
        z_order_back: get_text(d, "zorder") == "back",
    });
    let targets = if apply == 0 {
        (0..ed.sections().len()).collect()
    } else {
        ed.target_sections()
    };
    Ok((pb, targets))
}

// ---- the accept buttons -----------------------------------------------------

fn is_design(owner: DialogOwner) -> bool {
    matches!(
        owner,
        DialogOwner::DesignMoreColors
            | DialogOwner::DesignFillEffects
            | DialogOwner::DesignWatermark
            | DialogOwner::DesignPageBorders
    )
}

/// Press a button on a Design dialog: `None` for any other dialog (Options...
/// included: the stack writes it back), and for a cancel or open button,
/// which the generic stack handles. Accept closes the dialog before it
/// applies and reopens it with its staged values when the apply refuses.
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    if !is_design(top.owner) {
        return None;
    }
    let want = button.replace('&', "");
    let b = top
        .buttons
        .iter()
        .find(|b| b.label.replace('&', "").eq_ignore_ascii_case(want.trim()))?;
    if !b.enabled {
        return None;
    }
    let role = b.role;
    let dialog = top.clone();
    match role {
        ButtonRole::Accept => {
            tab.dialogs.pop();
            let done = apply(tab, &dialog);
            if done.is_err() {
                tab.dialogs.push(dialog);
            }
            Some(done)
        }
        ButtonRole::Apply => Some(apply(tab, &dialog)),
        _ => None,
    }
}

fn apply(tab: &mut DocTab, d: &Dialog) -> Result<(), String> {
    let (changed, status) = match d.owner {
        DialogOwner::DesignMoreColors => {
            let hex = get_text(d, "hex");
            let color = parse_hex(&hex)
                .ok_or_else(|| format!("Type a colour as six hex digits (RRGGBB); got '{hex}'"))?;
            let (_, pkg) = body(tab)?;
            let changed = pkg.set_page_background(Some(&PageBackground {
                color,
                gradient: None,
            }));
            (changed, format!("Page color: {}", color_name(color)))
        }
        DialogOwner::DesignFillEffects => {
            let two = get_choice(d, "colors") == 1;
            let color = chosen_color(d, "color1").unwrap_or(0xFFFFFF);
            let color2 = if two {
                chosen_color(d, "color2").unwrap_or(0xFFFFFF)
            } else {
                0xFFFFFF
            };
            let style = GradientStyle::ALL[get_choice(d, "style").min(4)];
            let (_, pkg) = body(tab)?;
            let changed = pkg.set_page_background(Some(&PageBackground {
                color,
                gradient: Some(Gradient { color2, style }),
            }));
            (changed, format!("Page color: {} gradient", style.label()))
        }
        DialogOwner::DesignWatermark => {
            let spec = watermark_spec(d)?;
            let changed = set_watermark(tab, spec.as_ref())?;
            let status = match &spec {
                Some(s) => format!("Watermark: {}", s.text),
                None => "Watermark removed".into(),
            };
            (changed, status)
        }
        DialogOwner::DesignPageBorders => {
            let ed = border_editor(tab)?;
            let (pb, targets) = page_borders_of(d, ed)?;
            let changed = ed.edit_sections(&targets, |raw| PageBorders::apply(pb.as_ref(), raw));
            let status = if pb.is_some() {
                "Page borders applied"
            } else {
                "Page borders removed"
            };
            (changed, status.to_string())
        }
        _ => return Err("not a Design dialog".into()),
    };
    tab.dirty |= changed;
    tab.status = status.into();
    Ok(())
}

#[cfg(test)]
mod tests;
