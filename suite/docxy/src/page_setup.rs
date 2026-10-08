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
use crate::dialog::catalog;
use crate::dialog::{
    Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Reaction, Value,
};
use crate::layout_tab::PageSetupTab;
use docxcore::sect::{Paper, SectionSetup, SectionStart, TWIPS_PER_INCH};

mod line_numbers;
pub(crate) use line_numbers::{apply_line_numbers, line_numbers_dialog};

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

pub(crate) fn choice(
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

pub(crate) fn ok_cancel() -> Vec<Button> {
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
    SectionSetup::parse(&caret_raw(ed))
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
        catalog::PAGE_SETUP,
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
        hidden("caret_sect", caret_raw(ed)),
        hidden("paper_pick", String::new()),
    ];
    d.buttons = ok_cancel();
    d.react = Some(Reaction(after_set));
    d.mark_opened();
    Ok(d)
}

/// The page no tab shows: a control kept here is the dialog's own state, not
/// drawn and not listed by `dialog-read`.
const HIDDEN_PAGE: usize = usize::MAX;

/// A control that holds state for the dialog's owner.
fn hidden(name: &'static str, value: String) -> Control {
    let mut c = Control::new(name, name, ControlKind::Label, Value::Text(value));
    c.visible = false;
    c.page = Some(HIDDEN_PAGE);
    c
}

/// The caret section's sectPr.
pub(crate) fn caret_raw(ed: &Editor) -> String {
    let sections = ed.sections();
    sections[ed.caret_section().min(sections.len() - 1)].clone()
}

fn index(d: &Dialog, name: &str) -> Option<usize> {
    d.controls.iter().position(|c| c.name == name)
}

pub(crate) fn text_of(d: &Dialog, name: &str) -> String {
    index(d, name).map_or_else(String::new, |i| d.controls[i].text())
}

pub(crate) fn chosen(d: &Dialog, name: &str) -> Option<usize> {
    match d.value(name) {
        Some(Value::Choice(i)) => *i,
        _ => None,
    }
}

/// Set a control's value only: the person's change, made through another.
fn set_value(d: &mut Dialog, name: &str, value: Value) {
    if let Some(i) = index(d, name) {
        d.controls[i].value = value;
    }
}

pub(crate) fn twips_of(text: &str) -> Option<i32> {
    let v = text.trim().parse::<f64>().ok().filter(|v| v.is_finite())?;
    Some((v * TWIPS_PER_INCH as f64).round() as i32)
}

// ---- the page: one rule for the dialog and OK ------------------------------
//
// OK does two things to a section's page, in this order:
// 1. the automatic part: when Orientation differs from what the dialog opened
//    on, `SectionSetup::set_landscape` (the margins rotate, and the sides swap
//    when their shape disagrees); then, when the person picked a named paper,
//    `set_paper` (kept in the orientation);
// 2. the person's part: each field (a margin, a distance, Width, Height) whose
//    shown value differs from what step 1 gives the caret section is written
//    as shown, and the paper code follows the resulting sides.
// The dialog reacts by the same rule: after an orientation change or a paper
// pick it shows step 1's result in every field the person has not changed, so
// for the caret section OK writes exactly what the dialog shows, and to any
// other section it adds only what the person changed.

/// Writes one page field into a section.
type Setter = fn(&mut SectionSetup, i32);

/// The fields of the page part, as (control, getter, setter).
type Field = (&'static str, fn(&SectionSetup) -> i32, Setter);

const FIELDS: [Field; 9] = [
    ("top", |s| s.margins.top, |s, v| s.margins.top = v),
    ("right", |s| s.margins.right, |s, v| s.margins.right = v),
    ("bottom", |s| s.margins.bottom, |s, v| s.margins.bottom = v),
    ("left", |s| s.margins.left, |s, v| s.margins.left = v),
    ("gutter", |s| s.margins.gutter, |s, v| s.margins.gutter = v),
    ("header", |s| s.margins.header, |s, v| s.margins.header = v),
    ("footer", |s| s.margins.footer, |s, v| s.margins.footer = v),
    ("width", |s| s.page.w, |s, v| s.page.w = v),
    ("height", |s| s.page.h, |s, v| s.page.h = v),
];

fn field_of(name: &str) -> &'static Field {
    FIELDS.iter().find(|f| f.0 == name).expect("a page field")
}

/// The orientation the dialog opened on.
fn opened_landscape(d: &Dialog) -> bool {
    index(d, "orientation").and_then(|i| d.opened.get(i)) == Some(&Value::Choice(Some(1)))
}

/// The named paper the person picked, if any.
fn picked(d: &Dialog) -> Option<Paper> {
    let pick = text_of(d, "paper_pick");
    Paper::ALL.into_iter().find(|p| p.label() == pick)
}

/// Step 1 for the caret section, for an orientation and a pick.
fn automatic(d: &Dialog, landscape: bool, pick: Option<Paper>) -> SectionSetup {
    let mut s = SectionSetup::parse(&text_of(d, "caret_sect"));
    apply_automatic(&mut s, landscape != opened_landscape(d), landscape, pick);
    s
}

/// Step 1 on any section.
fn apply_automatic(s: &mut SectionSetup, turn: bool, landscape: bool, pick: Option<Paper>) {
    if turn {
        s.set_landscape(landscape);
    }
    if let Some(p) = pick {
        s.page.set_paper(p);
    }
}

/// Whether the person changed a field: what the dialog shows is not what
/// step 1 gives the caret section.
fn person_changed(d: &Dialog, auto: &SectionSetup, name: &str) -> bool {
    text_of(d, name).trim() != inches((field_of(name).1)(auto))
}

/// Show step 1's result in every field the person has not changed, after
/// step 1 itself changed from `old` to `new`, and name the paper the sides
/// then show.
/// - `turned`: the page turned. Every margin moves, the person's too. The
///   sides swap together: by their own shown shape once the person has typed
///   one and both parse (as Word turns typed sides), else when step 1 swaps
///   them (by the section's own shape).
/// - `fill`: a paper was picked (or the opened one picked again). Both sides
///   show step 1's, whatever was typed.
/// - Otherwise each side, and the gutter and distances, that still shows
///   `old`'s value shows `new`'s.
fn follow(d: &mut Dialog, old: &SectionSetup, new: &SectionSetup, turned: bool, fill: bool) {
    if turned {
        let sides = ["top", "right", "bottom", "left"];
        let from = if new.page.landscape {
            ["left", "top", "right", "bottom"]
        } else {
            ["right", "bottom", "left", "top"]
        };
        let shown: Vec<String> = from.iter().map(|n| text_of(d, n)).collect();
        for (name, v) in sides.into_iter().zip(shown) {
            set_value(d, name, Value::Text(v));
        }
    }
    // Step 1 swaps the sides by the section's own shape. The shown sides
    // follow it while the person has typed neither; once they have, the
    // shown sides turn by their own shape, as Word's do, and the ones that
    // differ from step 1 are the person's.
    let auto_swapped =
        old.page.w != old.page.h && (new.page.w, new.page.h) == (old.page.h, old.page.w);
    let typed = ["width", "height"]
        .iter()
        .any(|n| text_of(d, n).trim() != inches((field_of(n).1)(old)));
    let shown = (
        twips_of(&text_of(d, "width")),
        twips_of(&text_of(d, "height")),
    );
    let swapped = match (typed && turned, shown) {
        (true, (Some(w), Some(h))) => w != h && (w > h) != new.page.landscape,
        _ => auto_swapped,
    };
    if fill {
        set_value(d, "width", Value::Text(inches(new.page.w)));
        set_value(d, "height", Value::Text(inches(new.page.h)));
    } else if swapped {
        let (w, h) = (text_of(d, "width"), text_of(d, "height"));
        set_value(d, "width", Value::Text(h));
        set_value(d, "height", Value::Text(w));
    } else {
        for name in ["width", "height"] {
            let get = field_of(name).1;
            if text_of(d, name).trim() == inches(get(old)) {
                set_value(d, name, Value::Text(inches(get(new))));
            }
        }
    }
    for name in ["gutter", "header", "footer"] {
        let get = field_of(name).1;
        if text_of(d, name).trim() == inches(get(old)) {
            set_value(d, name, Value::Text(inches(get(new))));
        }
    }
    show_paper(d);
}

/// The Paper size dropdown shows the named paper the shown sides match, else
/// Custom. It is the dialog's reading of the sides, never the person's pick.
/// The sides are matched as shown, to 0.01": A4's 8.27" stands for its
/// 11906 twips though it reads back as 11909.
fn show_paper(d: &mut Dialog) {
    let shown = |n: &str| twips_of(&text_of(d, n)).map(inches);
    let at = match (shown("width"), shown("height")) {
        (Some(w), Some(h)) => Paper::ALL
            .iter()
            .position(|p| {
                let (a, b) = p.size();
                let (a, b) = (inches(a), inches(b));
                (a == w && b == h) || (a == h && b == w)
            })
            .unwrap_or(Paper::ALL.len()),
        _ => Paper::ALL.len(),
    };
    set_value(d, "paper", Value::Choice(Some(at)));
}

/// Page Setup reacting to a change (`before` is the control's value before
/// it): an orientation change or a paper pick moves step 1 and the fields
/// the person has not changed follow it; typed sides rename the paper.
pub(crate) fn after_set(d: &mut Dialog, i: usize, before: &Value) {
    let landscape = |v: &Value| *v == Value::Choice(Some(1));
    match d.controls[i].name {
        "orientation" => {
            let (was, now) = (landscape(before), landscape(&d.controls[i].value));
            if was == now {
                return;
            }
            let pick = picked(d);
            let old = automatic(d, was, pick);
            let new = automatic(d, now, pick);
            follow(d, &old, &new, true, false);
        }
        "paper" => {
            let now = chosen(d, "orientation") == Some(1);
            let old = automatic(d, now, picked(d));
            let opened = index(d, "paper").and_then(|k| d.opened.get(k)).cloned();
            match chosen(d, "paper").and_then(|k| Paper::ALL.get(k).copied()) {
                // Custom: no named paper any more. The sides stay as shown, to
                // be typed, so the ones step 1 no longer gives are the
                // person's; the dropdown keeps Custom, and the next click on
                // it wraps round to the first paper.
                None => set_value(d, "paper_pick", Value::Text(String::new())),
                // The paper the dialog opened on: no pick, the section's own
                // sides again.
                Some(_) if Some(&d.controls[i].value) == opened.as_ref() => {
                    set_value(d, "paper_pick", Value::Text(String::new()));
                    let new = automatic(d, now, None);
                    follow(d, &old, &new, false, true);
                }
                Some(p) => {
                    set_value(d, "paper_pick", Value::Text(p.label().into()));
                    let new = automatic(d, now, Some(p));
                    follow(d, &old, &new, false, true);
                }
            }
        }
        "width" | "height" => show_paper(d),
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
/// editor, plus the settings it changed. Whether anything changed.
pub(crate) fn apply_page_setup(
    ed: &mut Editor,
    pkg: &mut Package,
    d: &Dialog,
) -> Result<bool, String> {
    // Every field must be a number. Top and bottom are signed in OOXML (a
    // negative one is an exact margin that text may not push), so only the
    // others refuse a negative value.
    let mut values = Vec::with_capacity(FIELDS.len());
    for f in &FIELDS {
        let v = field(d, f.0)?;
        if v < 0 && !matches!(f.0, "top" | "bottom") {
            let label = &d.controls[index(d, f.0).unwrap_or_default()].label;
            let what = label.replace('&', "");
            return Err(format!("{} cannot be negative", what.trim_end_matches(':')));
        }
        values.push((f, v));
    }
    let changed = |n: &str| d.changed(n);
    let landscape = chosen(d, "orientation") == Some(1);
    let turn = landscape != opened_landscape(d);
    let pick = picked(d);
    let auto = automatic(d, landscape, pick);
    let mine: Vec<(Setter, i32)> = values
        .iter()
        .filter(|(f, _)| person_changed(d, &auto, f.0))
        .map(|(f, v)| (f.2, *v))
        .collect();
    let sides_mine = ["width", "height"]
        .iter()
        .any(|n| person_changed(d, &auto, n));
    let start = chosen(d, "start").map(|i| STARTS[i].0);
    let gutter_at_top = chosen(d, "gutter_pos") == Some(1);
    let edit = |s: &mut SectionSetup| {
        apply_automatic(s, turn, landscape, pick);
        for (set, v) in &mine {
            set(s, *v);
        }
        if turn || pick.is_some() || sides_mine {
            s.page.code = Paper::matching(s.page.w, s.page.h).map(Paper::code);
        }
        if let (true, Some(start)) = (changed("start"), start) {
            s.start = start;
        }
    };
    // Check every section it would write before writing any.
    let sections = ed.sections();
    let targets = targets(ed, d);
    let check: Vec<usize> = match &targets {
        Some(k) => k.clone(),
        None => vec![ed.break_section()?],
    };
    for k in check {
        let mut s = SectionSetup::parse(&sections[k.min(sections.len() - 1)]);
        edit(&mut s);
        check_setup(&s, gutter_at_top)?;
    }
    let mut any = match targets {
        Some(k) => ed.edit_section_setups(&k, edit),
        None => {
            ed.insert_section_break_with(SectionStart::NextPage, edit)?;
            true
        }
    };
    if changed("gutter_pos") {
        pkg.set_gutter_at_top(gutter_at_top);
        any = true;
    }
    if changed("multiple") {
        pkg.set_mirror_margins(chosen(d, "multiple") == Some(1));
        any = true;
    }
    Ok(any)
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

// ---- Columns ----------------------------------------------------------------

/// The most columns Word lays out.
const MAX_COLUMNS: usize = 12;
/// How far explicit widths and spacings may miss the text width: 0.01".
const FIT_SLACK: i32 = 15;
/// Half of 0.01", in twips, rounded up: how far a shown value may be off.
const ROUNDING: i32 = 8;
const PRESETS: [&str; 5] = ["One", "Two", "Three", "Left", "Right"];
const WIDTH: [&str; MAX_COLUMNS] = [
    "width1", "width2", "width3", "width4", "width5", "width6", "width7", "width8", "width9",
    "width10", "width11", "width12",
];
const SPACE: [&str; MAX_COLUMNS] = [
    "space1", "space2", "space3", "space4", "space5", "space6", "space7", "space8", "space9",
    "space10", "space11", "space12",
];

/// The text width the dialog lays columns out in (a hidden control, kept in
/// twips).
fn text_width_of(d: &Dialog) -> i32 {
    text_of(d, "text_width").parse().unwrap_or(9360)
}

fn columns_count(d: &Dialog) -> usize {
    text_of(d, "num")
        .trim()
        .parse::<usize>()
        .unwrap_or(1)
        .clamp(1, MAX_COLUMNS)
}

fn is_on(d: &Dialog, name: &str) -> bool {
    d.value(name) == Some(&Value::Bool(true))
}

/// Show one row per column: a width for each, a spacing after all but the
/// last. With Equal column width on, only the first row takes input.
fn show_rows(d: &mut Dialog) {
    let n = columns_count(d);
    let equal = is_on(d, "equal");
    for k in 0..MAX_COLUMNS {
        for (names, shown) in [(&WIDTH, k < n), (&SPACE, k + 1 < n)] {
            if let Some(i) = index(d, names[k]) {
                d.controls[i].visible = shown;
                d.controls[i].enabled = k == 0 || !equal;
            }
        }
    }
}

/// Fill the rows with `n` equal columns `space` apart across the text width.
fn fill_equal(d: &mut Dialog, n: usize, space: i32) {
    let tw = text_width_of(d);
    let w = (tw - space * (n as i32 - 1)) / n as i32;
    for k in 0..MAX_COLUMNS {
        set_value(d, WIDTH[k], Value::Text(inches(w)));
        set_value(d, SPACE[k], Value::Text(inches(space)));
    }
}

/// The preset the rows match, if any.
fn matching_preset(d: &Dialog) -> Option<usize> {
    let n = columns_count(d);
    if is_on(d, "equal") {
        return (n <= 3).then(|| n - 1);
    }
    let w = |k: usize| twips_of(&text_of(d, WIDTH[k])).unwrap_or(0);
    (n == 2 && w(0) != w(1)).then(|| if w(0) < w(1) { 3 } else { 4 })
}

/// The Columns dialog reacting to a change, as Word's does: a preset sets the
/// number and widths; the number of columns or Equal column width spreads
/// the columns evenly; with equal columns, column 1's width or spacing sets
/// every column's.
pub(crate) fn after_columns_set(d: &mut Dialog, i: usize, _before: &Value) {
    let name = d.controls[i].name;
    let space = || 720;
    match name {
        "preset" => {
            let Some(p) = chosen(d, "preset") else {
                return;
            };
            let tw = text_width_of(d);
            if p < 3 {
                set_value(d, "num", Value::Text((p + 1).to_string()));
                set_value(d, "equal", Value::Bool(true));
                fill_equal(d, p + 1, space());
            } else {
                let cols = docxcore::sect::Columns::default().two_unequal(tw, p == 3);
                set_value(d, "num", Value::Text("2".into()));
                set_value(d, "equal", Value::Bool(false));
                for (k, c) in cols.cols.iter().enumerate() {
                    set_value(d, WIDTH[k], Value::Text(inches(c.w)));
                    set_value(d, SPACE[k], Value::Text(inches(c.space)));
                }
            }
        }
        "num" | "equal" => {
            let space = twips_of(&text_of(d, "space1")).unwrap_or(space());
            let n = columns_count(d);
            fill_equal(d, n, if n > 1 { space } else { 720 });
        }
        "width1" | "space1" if is_on(d, "equal") => {
            let n = columns_count(d) as i32;
            let tw = text_width_of(d);
            let (Some(w), Some(s)) = (
                twips_of(&text_of(d, "width1")),
                twips_of(&text_of(d, "space1")),
            ) else {
                return;
            };
            let (w, s) = if name == "width1" && n > 1 {
                (w, (tw - n * w) / (n - 1))
            } else {
                ((tw - s * (n - 1)) / n, s)
            };
            for k in 0..MAX_COLUMNS {
                if k > 0 || name != "width1" {
                    set_value(d, WIDTH[k], Value::Text(inches(w)));
                }
                if k > 0 || name != "space1" {
                    set_value(d, SPACE[k], Value::Text(inches(s)));
                }
            }
        }
        _ => {}
    }
    show_rows(d);
    if name != "preset" {
        let p = matching_preset(d);
        set_value(d, "preset", Value::Choice(p));
    }
}

/// The Columns dialog on the caret section's columns.
pub(crate) fn columns_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let (ed, pkg) = body_of(tab)?;
    let s = caret_setup(ed);
    let tw = s.text_width(pkg.has_gutter_at_top());
    let c = &s.columns;
    let n = c.count() as usize;
    let mut d = Dialog::message(
        catalog::COLUMNS,
        "Columns",
        String::new(),
        &[],
        DialogOwner::Columns,
    );
    d.text = None;
    d.controls = vec![
        choice(
            "preset",
            "Presets:",
            ControlKind::Radio,
            &PRESETS,
            None,
            None,
        ),
        Control::new(
            "num",
            "&Number of columns:",
            ControlKind::Number,
            Value::Text(n.to_string()),
        ),
    ];
    let equal_w = (tw - c.space * (n as i32 - 1)) / n as i32;
    for k in 0..MAX_COLUMNS {
        let (w, sp) = match c.cols.get(k) {
            Some(col) => (col.w, col.space),
            None => (equal_w, c.space),
        };
        d.controls.push(Control::new(
            WIDTH[k],
            &format!("Width {}:", k + 1),
            ControlKind::Number,
            Value::Text(inches(w)),
        ));
        d.controls.push(Control::new(
            SPACE[k],
            &format!("Spacing {}:", k + 1),
            ControlKind::Number,
            Value::Text(inches(sp)),
        ));
    }
    d.controls.extend([
        Control::new(
            "equal",
            "&Equal column width",
            ControlKind::Checkbox,
            Value::Bool(c.equal_width()),
        ),
        Control::new(
            "sep",
            "Line &between",
            ControlKind::Checkbox,
            Value::Bool(c.sep),
        ),
        apply_to(ed),
        hidden("text_width", tw.to_string()),
    ]);
    show_rows(&mut d);
    let p = matching_preset(&d);
    set_value(&mut d, "preset", Value::Choice(p));
    d.buttons = ok_cancel();
    d.react = Some(Reaction(after_columns_set));
    d.mark_opened();
    Ok(d)
}

/// Apply an accepted Columns dialog: one undo step on the body editor. This
/// point forward starts the new section with a Continuous break. Whether
/// anything changed.
pub(crate) fn apply_columns(
    ed: &mut Editor,
    pkg: &mut Package,
    d: &Dialog,
) -> Result<bool, String> {
    let n: usize = text_of(d, "num")
        .trim()
        .parse()
        .ok()
        .filter(|n| (1..=MAX_COLUMNS).contains(n))
        .ok_or("Number of columns must be a whole number from 1 to 12")?;
    let equal = is_on(d, "equal");
    let sep = is_on(d, "sep");
    let widths = (0..n)
        .map(|k| field(d, WIDTH[k]))
        .collect::<Result<Vec<_>, _>>()?;
    let spaces = (0..n)
        .map(|k| if k + 1 < n { field(d, SPACE[k]) } else { Ok(0) })
        .collect::<Result<Vec<_>, _>>()?;
    if widths.iter().chain(&spaces).any(|v| *v < 0) {
        return Err("Widths and spacings cannot be negative".into());
    }
    let layout_changed = ["preset", "num", "equal"]
        .into_iter()
        .chain(WIDTH)
        .chain(SPACE)
        .any(|name| d.changed(name));
    // Check the layout in every section it goes to, before writing any: the
    // section a This point forward break lands in, else each target. Equal
    // columns write only their number and spacing, and Word sizes them to
    // each section, so they only need room for the spacing; unequal ones
    // must add up to the section's text width.
    let targets_now = targets(ed, d);
    let check: Vec<usize> = match &targets_now {
        Some(k) => k.clone(),
        None => vec![ed.break_section()?],
    };
    let sections = ed.sections();
    let total: i32 = widths.iter().sum::<i32>() + spaces.iter().sum::<i32>();
    // Each value is shown to 0.01", so it may be up to half of that off the
    // twips it stands for: allow that on top of the 0.01" slack.
    let slack = FIT_SLACK + ROUNDING * (2 * n as i32 - 1);
    let checked = if layout_changed && n > 1 {
        check.as_slice()
    } else {
        &[]
    };
    for &k in checked {
        let tw = SectionSetup::parse(&sections[k.min(sections.len() - 1)])
            .text_width(pkg.has_gutter_at_top());
        let named = |e: String| {
            if check.len() > 1 {
                format!("Section {}: {e}", k + 1)
            } else {
                e
            }
        };
        if equal {
            if spaces[0] * (n as i32 - 1) >= tw {
                return Err(named(format!(
                    "The spacing takes {}\" but the text is {}\" wide",
                    inches(spaces[0] * (n as i32 - 1)),
                    inches(tw)
                )));
            }
        } else if (total - tw).abs() > slack {
            return Err(named(format!(
                "The columns take {}\" but the text is {}\" wide",
                inches(total),
                inches(tw)
            )));
        }
    }
    let sep_changed = d.changed("sep");
    let space = if n > 1 { spaces[0] } else { 720 };
    let cols = docxcore::sect::Columns {
        num: n as i32,
        space,
        sep,
        cols: if equal || n == 1 {
            Vec::new()
        } else {
            widths
                .iter()
                .zip(&spaces)
                .map(|(&w, &space)| docxcore::sect::Column { w, space })
                .collect()
        },
    };
    let edit = |s: &mut SectionSetup| {
        if layout_changed {
            s.columns = docxcore::sect::Columns {
                sep: s.columns.sep,
                ..cols.clone()
            };
        }
        if sep_changed {
            s.columns.sep = sep;
        }
    };
    Ok(match targets_now {
        Some(k) => ed.edit_section_setups(&k, edit),
        None => {
            ed.insert_section_break_with(SectionStart::Continuous, edit)?;
            true
        }
    })
}

#[cfg(test)]
mod tests;
