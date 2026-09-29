//! Word's Page Setup and Columns dialogs (#649), opened from the Layout tab.
//!
//! Each dialog opens on the caret section's values. OK applies to the sections
//! "Apply to" names, as one undo step on the body editor:
//! - This section (Selected sections when the selection spans several), the
//!   caret's sections;
//! - This point forward, a new section from the caret, started by a section
//!   break (Next Page for Page Setup, Continuous for Columns);
//! - Whole document, every section.
//!
//! OK writes only the fields the person changed since the dialog opened, so
//! applying Landscape to the whole document rotates each section's own
//! margins instead of copying the caret section's onto every one. A value out
//! of range refuses OK with the reason, and the dialog stays open.
use super::*;
use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::layout_tab::PageSetupTab;
use docxcore::sect::{Paper, SectionSetup, SectionStart, TWIPS_PER_INCH};

const CUSTOM: &str = "Custom";
const THIS_SECTION: &str = "This section";
const SELECTED_SECTIONS: &str = "Selected sections";
const FORWARD: &str = "This point forward";
const WHOLE: &str = "Whole document";
const STARTS: [(SectionStart, &str); 4] = [
    (SectionStart::NextPage, "New page"),
    (SectionStart::Continuous, "Continuous"),
    (SectionStart::EvenPage, "Even page"),
    (SectionStart::OddPage, "Odd page"),
];
/// Page sides Word accepts: 0.1" to 22".
const MIN_SIDE: i32 = 144;
const MAX_SIDE: i32 = 22 * TWIPS_PER_INCH;
/// The least text width or height a page may keep: 0.1".
const MIN_TEXT: i32 = 144;

/// Twips as the dialog shows them: inches, to two places, without trailing
/// zeros (1440 is "1", 1800 is "1.25").
pub(crate) fn inches(twips: i32) -> String {
    let s = format!("{:.2}", twips as f64 / TWIPS_PER_INCH as f64);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn number(name: &'static str, label: &str, twips: i32, page: Option<usize>) -> Control {
    let mut c = Control::new(name, label, ControlKind::Number, Value::Text(inches(twips)));
    c.page = page;
    c
}

fn choice(
    name: &'static str,
    label: &str,
    kind: ControlKind,
    items: &[&str],
    chosen: Option<usize>,
    page: Option<usize>,
) -> Control {
    let mut c = Control::new(name, label, kind, Value::Choice(chosen));
    c.items = items.iter().map(|s| s.to_string()).collect();
    c.page = page;
    c
}

fn ok_cancel() -> Vec<Button> {
    vec![
        Button {
            default: true,
            ..Button::new("OK", ButtonRole::Accept)
        },
        Button::new("Cancel", ButtonRole::Cancel),
    ]
}

/// The body editor and package of a .docx tab.
fn body_of(tab: &DocTab) -> Result<(&Editor, &Package), String> {
    match (&tab.surface, tab.pkg.as_ref()) {
        (Surface::Doc(ed), Some(pkg)) => Ok((ed, pkg)),
        (Surface::Doc(_), None) => Err("Page layout needs a .docx (not Markdown)".into()),
        _ => Err("Page layout needs a document".into()),
    }
}

/// The caret section's setup.
fn caret_setup(ed: &Editor) -> SectionSetup {
    let sections = ed.sections();
    SectionSetup::parse(&sections[ed.caret_section().min(sections.len() - 1)])
}

/// "Apply to"'s choices: Word's, with Selected sections in place of This
/// section when the selection spans several, and only Whole document and
/// This point forward in a one-section document.
fn apply_to(ed: &Editor) -> Control {
    let items: Vec<&str> = if ed.sections().len() == 1 {
        vec![WHOLE, FORWARD]
    } else if ed.target_sections().len() > 1 {
        vec![SELECTED_SECTIONS, FORWARD, WHOLE]
    } else {
        vec![THIS_SECTION, FORWARD, WHOLE]
    };
    choice(
        "apply",
        "Apply to:",
        ControlKind::Dropdown,
        &items,
        Some(0),
        None,
    )
}

/// The Page Setup dialog on the caret section's values, open on `page`.
pub(crate) fn page_setup_dialog(tab: &DocTab, page: PageSetupTab) -> Result<Dialog, String> {
    let (ed, pkg) = body_of(tab)?;
    let s = caret_setup(ed);
    let m = s.margins;
    let paper = Paper::matching(s.page.w, s.page.h);
    let mut papers: Vec<&str> = Paper::ALL.iter().map(|p| p.label()).collect();
    papers.push(CUSTOM);
    let paper_at = paper.map_or(papers.len() - 1, |p| {
        Paper::ALL.iter().position(|q| *q == p).unwrap_or(0)
    });
    let start_at = STARTS.iter().position(|(st, _)| *st == s.start);
    let mut d = Dialog::message(
        "page-setup",
        "Page Setup",
        String::new(),
        &[],
        DialogOwner::PageSetup,
    );
    d.text = None;
    d.tabs = vec!["Margins".into(), "Paper".into(), "Layout".into()];
    d.tab = match page {
        PageSetupTab::Margins => 0,
        PageSetupTab::Paper => 1,
    };
    d.controls = vec![
        number("top", "&Top:", m.top, Some(0)),
        number("bottom", "&Bottom:", m.bottom, Some(0)),
        number("left", "&Left:", m.left, Some(0)),
        number("right", "&Right:", m.right, Some(0)),
        number("gutter", "&Gutter:", m.gutter, Some(0)),
        choice(
            "gutter_pos",
            "Gutter position:",
            ControlKind::Radio,
            &["Left", "Top"],
            Some(usize::from(pkg.has_gutter_at_top())),
            Some(0),
        ),
        choice(
            "orientation",
            "Orientation:",
            ControlKind::Radio,
            &["Portrait", "Landscape"],
            Some(usize::from(s.page.landscape)),
            Some(0),
        ),
        choice(
            "multiple",
            "Multiple pages:",
            ControlKind::Dropdown,
            &["Normal", "Mirror margins"],
            Some(usize::from(pkg.has_mirror_margins())),
            Some(0),
        ),
        choice(
            "paper",
            "Paper size:",
            ControlKind::Dropdown,
            &papers,
            Some(paper_at),
            Some(1),
        ),
        number("width", "&Width:", s.page.w, Some(1)),
        number("height", "&Height:", s.page.h, Some(1)),
        choice(
            "start",
            "Section start:",
            ControlKind::Dropdown,
            &STARTS.map(|(_, l)| l),
            start_at,
            Some(2),
        ),
        number("header", "Header from edge:", m.header, Some(2)),
        number("footer", "Footer from edge:", m.footer, Some(2)),
        apply_to(ed),
    ];
    d.buttons = ok_cancel();
    d.mark_opened();
    Ok(d)
}

fn index(d: &Dialog, name: &str) -> Option<usize> {
    d.controls.iter().position(|c| c.name == name)
}

fn text_of(d: &Dialog, name: &str) -> String {
    index(d, name).map_or_else(String::new, |i| d.controls[i].text())
}

fn chosen(d: &Dialog, name: &str) -> Option<usize> {
    match d.value(name) {
        Some(Value::Choice(i)) => *i,
        _ => None,
    }
}

/// Set a control's value and the value it opened on together, so a change
/// the dialog makes itself does not count as the person's.
fn set_both(d: &mut Dialog, name: &str, value: Value) {
    let Some(i) = index(d, name) else {
        return;
    };
    let same = d.opened.get(i) == Some(&d.controls[i].value);
    d.controls[i].value = value.clone();
    if same && let Some(o) = d.opened.get_mut(i) {
        *o = value;
    }
}

/// Set a control's value only: the person's change, made through another.
fn set_value(d: &mut Dialog, name: &str, value: Value) {
    if let Some(i) = index(d, name) {
        d.controls[i].value = value;
    }
}

fn twips_of(text: &str) -> Option<i32> {
    let v = text.trim().parse::<f64>().ok().filter(|v| v.is_finite())?;
    Some((v * TWIPS_PER_INCH as f64).round() as i32)
}

/// Page Setup reacting to a change, as Word's does:
/// - Orientation turns the page: Width and Height swap, and the margins
///   rotate as [`SectionSetup::set_landscape`] does.
/// - A named paper size fills in Width and Height.
/// - Width or Height names the paper they match, else Custom.
pub(crate) fn after_set(d: &mut Dialog, i: usize) {
    match d.controls[i].name {
        "orientation" => {
            let landscape = chosen(d, "orientation") == Some(1);
            let (Some(w), Some(h)) = (
                twips_of(&text_of(d, "width")),
                twips_of(&text_of(d, "height")),
            ) else {
                return;
            };
            if (w > h) == landscape || w == h {
                return;
            }
            set_both(d, "width", Value::Text(inches(h)));
            set_both(d, "height", Value::Text(inches(w)));
            let [top, right, bottom, left] =
                ["top", "right", "bottom", "left"].map(|n| text_of(d, n));
            let turned = if landscape {
                [left, top, right, bottom]
            } else {
                [right, bottom, left, top]
            };
            for (name, v) in ["top", "right", "bottom", "left"].into_iter().zip(turned) {
                set_both(d, name, Value::Text(v));
            }
        }
        "paper" => {
            let Some(p) = chosen(d, "paper").and_then(|i| Paper::ALL.get(i).copied()) else {
                return;
            };
            let (w, h) = p.size();
            let landscape = chosen(d, "orientation") == Some(1);
            let (w, h) = if landscape { (h, w) } else { (w, h) };
            set_value(d, "width", Value::Text(inches(w)));
            set_value(d, "height", Value::Text(inches(h)));
        }
        "width" | "height" => {
            let (Some(w), Some(h)) = (
                twips_of(&text_of(d, "width")),
                twips_of(&text_of(d, "height")),
            ) else {
                return;
            };
            let at = Paper::matching(w, h)
                .and_then(|p| Paper::ALL.iter().position(|q| *q == p))
                .unwrap_or(Paper::ALL.len());
            set_value(d, "paper", Value::Choice(Some(at)));
        }
        _ => {}
    }
}

/// A number field's value in twips, or why OK refuses it.
fn field(d: &Dialog, name: &str) -> Result<i32, String> {
    let c = &d.controls[index(d, name).ok_or_else(|| format!("no field {name}"))?];
    twips_of(&c.text()).ok_or_else(|| format!("{} takes a number", c.label.replace('&', "")))
}

/// Which sections "Apply to" names; `None` for This point forward.
fn targets(ed: &Editor, d: &Dialog) -> Option<Vec<usize>> {
    match text_of(d, "apply").as_str() {
        FORWARD => None,
        WHOLE => Some((0..ed.sections().len()).collect()),
        _ => Some(ed.target_sections()),
    }
}

/// Apply an accepted Page Setup to the document: one undo step on the body
/// editor, plus the settings it changed.
pub(crate) fn apply_page_setup(
    ed: &mut Editor,
    pkg: &mut Package,
    d: &Dialog,
) -> Result<(), String> {
    let [
        top,
        bottom,
        left,
        right,
        gutter,
        width,
        height,
        header,
        footer,
    ] = [
        "top", "bottom", "left", "right", "gutter", "width", "height", "header", "footer",
    ]
    .map(|n| field(d, n));
    let (top, bottom, left, right, gutter) = (top?, bottom?, left?, right?, gutter?);
    let (width, height, header, footer) = (width?, height?, header?, footer?);
    for (v, what) in [
        (top, "Top"),
        (bottom, "Bottom"),
        (left, "Left"),
        (right, "Right"),
        (gutter, "Gutter"),
        (header, "Header"),
        (footer, "Footer"),
    ] {
        if v < 0 {
            return Err(format!("{what} cannot be negative"));
        }
    }
    let changed = |n: &str| d.changed(n);
    let landscape = chosen(d, "orientation") == Some(1);
    let paper = chosen(d, "paper").and_then(|i| Paper::ALL.get(i).copied());
    let start = chosen(d, "start").map(|i| STARTS[i].0);
    let gutter_at_top = chosen(d, "gutter_pos") == Some(1);
    let edit = |s: &mut SectionSetup| {
        if changed("orientation") {
            s.set_landscape(landscape);
        }
        match paper {
            Some(p) if changed("paper") => s.page.set_paper(p),
            _ if changed("width") || changed("height") || changed("paper") => {
                s.page.set_custom(width, height)
            }
            _ => {}
        }
        for (name, slot, v) in [
            ("top", &mut s.margins.top, top),
            ("bottom", &mut s.margins.bottom, bottom),
            ("left", &mut s.margins.left, left),
            ("right", &mut s.margins.right, right),
            ("gutter", &mut s.margins.gutter, gutter),
            ("header", &mut s.margins.header, header),
            ("footer", &mut s.margins.footer, footer),
        ] {
            if changed(name) {
                *slot = v;
            }
        }
        if let (true, Some(start)) = (changed("start"), start) {
            s.start = start;
        }
    };
    // Check every section it would write before writing any.
    let sections = ed.sections();
    let targets = targets(ed, d);
    let check: Vec<usize> = targets.clone().unwrap_or_else(|| vec![ed.caret_section()]);
    for k in check {
        let mut s = SectionSetup::parse(&sections[k.min(sections.len() - 1)]);
        edit(&mut s);
        check_setup(&s, gutter_at_top)?;
    }
    match targets {
        Some(k) => {
            ed.edit_section_setups(&k, edit);
        }
        None => ed.insert_section_break_with(SectionStart::NextPage, edit)?,
    }
    if changed("gutter_pos") {
        pkg.set_gutter_at_top(gutter_at_top);
    }
    if changed("multiple") {
        pkg.set_mirror_margins(chosen(d, "multiple") == Some(1));
    }
    Ok(())
}

/// Refuse a page Word would not lay out.
fn check_setup(s: &SectionSetup, gutter_at_top: bool) -> Result<(), String> {
    for side in [s.page.w, s.page.h] {
        if !(MIN_SIDE..=MAX_SIDE).contains(&side) {
            return Err(format!(
                "The paper must be between {} and {} inches on each side",
                inches(MIN_SIDE),
                inches(MAX_SIDE)
            ));
        }
    }
    if s.text_width(gutter_at_top) < MIN_TEXT || s.text_height(gutter_at_top) < MIN_TEXT {
        return Err("The margins leave no room for text on the page".into());
    }
    Ok(())
}

/// Columns reacting to a change (the Columns dialog, below).
pub(crate) fn after_columns_set(_d: &mut Dialog, _i: usize) {}

/// The Columns dialog (step 6).
pub(crate) fn columns_dialog(_tab: &DocTab) -> Result<Dialog, String> {
    Err("This dialog is not available yet".into())
}

pub(crate) fn apply_columns(
    _ed: &mut Editor,
    _pkg: &mut Package,
    _d: &Dialog,
) -> Result<(), String> {
    Err("This dialog is not available yet".into())
}

#[cfg(test)]
mod tests;
