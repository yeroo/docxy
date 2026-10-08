//! Page Layout › Page Setup, Scale to Fit and Sheet Options (#1019), over
//! [`gridcore::print`]: print area, page breaks, margins, orientation, paper
//! size, scaling, the print options, and the Page Setup dialog.
//!
//! Every command acts on the active sheet and is one undo step when it
//! changed something. A command that changes nothing (Clear Print Area with
//! none, Portrait on a portrait sheet) pushes no step and leaves the tab
//! clean. Page setup is not a `<sheetProtection>` flag and a print area is a
//! defined name, so, as in Excel, a protected sheet takes them all.
//!
//! The Page Setup dialog is one form on the tab's
//! [`crate::dialog::DialogStack`], so the harness's `dialog-read`,
//! `dialog-set` and `dialog-click` drive it. A value Excel refuses keeps the
//! dialog open, says why and changes nothing.

use crate::dialog::{ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{DocTab, SheetView, Surface};
use gridcore::print::area::{self, PrintRef, PrintTitles};
use gridcore::print::setup::{Margins, Orientation, PageSetup};
use gridcore::sheet::Workbook;

/// Page Layout › Print Area's items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AreaOp {
    Set,
    Clear,
    Add,
}

/// Page Layout › Breaks' items, at the active cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BreakOp {
    Insert,
    Remove,
    Reset,
}

/// Page Layout › Margins' presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarginPreset {
    Normal,
    Wide,
    Narrow,
}

impl MarginPreset {
    /// Excel's margins for the preset, in inches.
    pub fn margins(self) -> Margins {
        let m = |left_right: f64, top_bottom: f64, header_footer: f64| Margins {
            left: left_right,
            right: left_right,
            top: top_bottom,
            bottom: top_bottom,
            header: header_footer,
            footer: header_footer,
        };
        match self {
            MarginPreset::Normal => Margins::default(),
            MarginPreset::Wide => m(1.0, 1.0, 0.5),
            MarginPreset::Narrow => m(0.25, 0.75, 0.3),
        }
    }
}

/// A Page Layout command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageAct {
    Area(AreaOp),
    Break(BreakOp),
    /// Orientation › Landscape (`true`) or Portrait.
    Landscape(bool),
    /// A Size item: a `paperSize` code.
    Paper(u32),
    Margins(MarginPreset),
    /// Scale to Fit › Width: and Height: (0 is Automatic).
    FitWidth(u32),
    FitHeight(u32),
    /// Scale to Fit › Scale:, a percentage.
    Scale(u32),
    /// Sheet Options › Print Gridlines and Print Headings: toggles.
    PrintGridlines,
    PrintHeadings,
    /// The Page Setup dialog, open on a tab: Custom Margins..., More Paper
    /// Sizes..., Print Titles, More Pages..., Custom... and the groups'
    /// launchers.
    Dialog(SetupTab),
}

impl PageAct {
    /// Whether it reads the selection: Print Area takes its areas, Breaks
    /// the active cell. The rest are the sheet's.
    pub fn targets_cells(self) -> bool {
        matches!(self, PageAct::Area(_) | PageAct::Break(_))
    }
}

/// The Size menu's papers: `paperSize` codes and their names.
pub(crate) const PAPERS: &[(u32, &str)] = &[
    (1, "Letter"),
    (3, "Tabloid"),
    (5, "Legal"),
    (7, "Executive"),
    (8, "A3"),
    (9, "A4"),
    (11, "A5"),
    (12, "B4 (JIS)"),
    (13, "B5 (JIS)"),
];

/// Scale to Fit › Width: and Height:'s page counts after Automatic.
pub(crate) const FIT_PAGES: std::ops::RangeInclusive<u32> = 1..=9;

/// Scale to Fit › Scale:'s percentages.
pub(crate) const SCALES: &[u32] = &[50, 75, 100, 125, 150, 200];

/// The (width, height) in pages Scale to Fit shows: Automatic (0) for both
/// while the sheet prints at a scale.
pub(crate) fn shown_fit(p: &PageSetup) -> (u32, u32) {
    if p.fit_to_page {
        (p.fit_width, p.fit_height)
    } else {
        (0, 0)
    }
}

/// Whether `act` reads as on for sheet `s`: a ticked menu item or a checked
/// box. Orientation's `default` prints portrait, so Portrait is ticked.
pub(crate) fn is_on(act: PageAct, wb: &Workbook, s: usize) -> bool {
    let Some(sheet) = wb.sheets.get(s) else {
        return false;
    };
    let p = &sheet.page_setup;
    match act {
        PageAct::Landscape(l) => p.orientation.is_landscape() == l,
        PageAct::Paper(code) => p.paper_size == code,
        PageAct::Margins(m) => p.margins == m.margins(),
        PageAct::FitWidth(n) => shown_fit(p).0 == n,
        PageAct::FitHeight(n) => shown_fit(p).1 == n,
        PageAct::Scale(n) => !p.fit_to_page && p.scale == n,
        PageAct::PrintGridlines => p.grid_lines,
        PageAct::PrintHeadings => p.headings,
        PageAct::Area(_) | PageAct::Break(_) | PageAct::Dialog(_) => false,
    }
}

/// Scale to Fit's width and height (`None` keeps one): a page count fits
/// the sheet to pages; both Automatic prints at the scale again.
fn set_fit(p: &mut PageSetup, w: Option<u32>, h: Option<u32>) {
    let (mut cw, mut ch) = shown_fit(p);
    if let Some(w) = w {
        cw = w;
    }
    if let Some(h) = h {
        ch = h;
    }
    if (cw, ch) == (0, 0) {
        p.fit_to_page = false;
    } else {
        p.fit_to_page = true;
        p.fit_width = cw;
        p.fit_height = ch;
    }
}

/// What a page command can change: the sheet's page setup and breaks, and
/// the workbook's defined names (print area and titles).
type LayoutState = (
    PageSetup,
    Vec<gridcore::sheet::PageBreak>,
    Vec<gridcore::sheet::PageBreak>,
    Vec<gridcore::sheet::DefinedName>,
);

fn layout_state(wb: &Workbook, s: usize) -> LayoutState {
    let sheet = &wb.sheets[s];
    (
        sheet.page_setup.clone(),
        sheet.row_breaks.clone(),
        sheet.col_breaks.clone(),
        wb.defined_names.clone(),
    )
}

/// Run `op` on the active sheet as one undo step, taken only when it changed
/// the layout; the tab is dirtied then too. `Ok(false)`: nothing changed. An
/// `Err` from `op` must come before it changes anything.
fn edit(
    tab: &mut DocTab,
    op: impl FnOnce(&mut SheetView, usize) -> Result<(), String>,
) -> Result<bool, String> {
    let Surface::Sheet(v) = &mut tab.surface else {
        return Err("page setup needs a spreadsheet".into());
    };
    let s = v.active;
    let snap = v.snapshot();
    let before = layout_state(&v.pkg.workbook, s);
    op(v, s)?;
    if layout_state(&v.pkg.workbook, s) == before {
        return Ok(false);
    }
    v.push_undo_snapshot(snap);
    tab.set_dirty();
    Ok(true)
}

/// Excel's Add to Print Area extends a list of ranges; a print area defined
/// any other way (`OFFSET(...)`, `INDIRECT(...)`) has nothing to add to.
pub(crate) const ADD_REFUSED: &str = "The print area is not a list of ranges, so nothing can be added to it. Set Print Area replaces it.";

/// Whether the sheet's print area, if any, is the list of ranges it reads as:
/// spelling those ranges again gives the definition back.
fn print_area_is_ranges(wb: &Workbook, s: usize) -> bool {
    let Some(d) = wb
        .defined_names
        .iter()
        .find(|d| d.name == area::PRINT_AREA && d.scope == Some(s))
    else {
        return true;
    };
    let f = d.formula.trim().trim_start_matches('=');
    area::spell_refs(&wb.sheets[s].name, &area::parse_refs(f)) == f
}

/// Apply one command to `v`'s sheet `s`. A refusal comes before any change.
fn apply(v: &mut SheetView, s: usize, act: PageAct) -> Result<(), String> {
    let areas = v.areas_all();
    let (row, col) = v.sel;
    // A choice already shown leaves the sheet as it is: Portrait on a
    // `default` sheet does not write `portrait`. The print options toggle.
    let shown = is_on(act, &v.pkg.workbook, s)
        && !matches!(act, PageAct::PrintGridlines | PageAct::PrintHeadings);
    let wb = &mut v.pkg.workbook;
    match act {
        PageAct::Area(AreaOp::Set) => area::set_print_area(wb, s, &areas),
        PageAct::Area(AreaOp::Add) => {
            if !print_area_is_ranges(wb, s) {
                return Err(ADD_REFUSED.into());
            }
            for a in areas {
                area::add_print_area(wb, s, a);
            }
        }
        PageAct::Area(AreaOp::Clear) => {
            area::clear_print_area(wb, s);
        }
        PageAct::Break(op) => {
            let sheet = &mut wb.sheets[s];
            match op {
                BreakOp::Insert => area::insert_page_break(sheet, row, col),
                BreakOp::Remove => area::remove_page_break(sheet, row, col),
                BreakOp::Reset => area::reset_page_breaks(sheet),
            };
        }
        _ if shown => {}
        _ => {
            let p = &mut wb.sheets[s].page_setup;
            match act {
                PageAct::Landscape(true) => p.orientation = Orientation::Landscape,
                PageAct::Landscape(false) => p.orientation = Orientation::Portrait,
                PageAct::Paper(code) => p.paper_size = code,
                PageAct::Margins(m) => p.margins = m.margins(),
                PageAct::FitWidth(n) => set_fit(p, Some(n), None),
                PageAct::FitHeight(n) => set_fit(p, None, Some(n)),
                PageAct::Scale(n) => {
                    p.fit_to_page = false;
                    p.scale = n;
                }
                PageAct::PrintGridlines => p.grid_lines = !p.grid_lines,
                PageAct::PrintHeadings => p.headings = !p.headings,
                PageAct::Area(_) | PageAct::Break(_) | PageAct::Dialog(_) => {}
            }
        }
    }
    Ok(())
}

/// Run a Page Layout command on the tab.
pub(crate) fn run(tab: &mut DocTab, act: PageAct) {
    if let PageAct::Dialog(at) = act {
        match dialog(tab, at) {
            Ok(d) => tab.dialogs.push(d),
            Err(e) => tab.status = e.into(),
        }
        return;
    }
    if let Err(e) = edit(tab, |v, s| apply(v, s, act)) {
        tab.status = e.into();
    }
}

/// The Page Setup dialog's tabs; a command opens it on the one it is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupTab {
    /// Orientation, paper and scaling: More Paper Sizes..., the Page Setup
    /// and Scale to Fit launchers.
    Page,
    /// Custom Margins....
    Margins,
    /// Print area, titles and the print options: Print Titles and the Sheet
    /// Options launcher.
    Sheet,
}

impl SetupTab {
    const NAMES: [&str; 3] = ["Page", "Margins", "Sheet"];

    fn index(self) -> usize {
        match self {
            SetupTab::Page => 0,
            SetupTab::Margins => 1,
            SetupTab::Sheet => 2,
        }
    }
}

/// The Page Setup dialog's orientation and scaling choices.
const ORIENTATIONS: [&str; 2] = ["Portrait", "Landscape"];
const SCALING: [&str; 2] = ["Adjust to", "Fit to"];
/// The margin fields, in the order of [`margin_fields`].
const MARGINS: [(&str, &str); 6] = [
    ("top", "Top:"),
    ("bottom", "Bottom:"),
    ("left", "Left:"),
    ("right", "Right:"),
    ("header", "Header:"),
    ("footer", "Footer:"),
];

fn margin_fields(m: &Margins) -> [f64; 6] {
    [m.top, m.bottom, m.left, m.right, m.header, m.footer]
}

fn margin_mut<'a>(m: &'a mut Margins, name: &str) -> &'a mut f64 {
    match name {
        "top" => &mut m.top,
        "bottom" => &mut m.bottom,
        "left" => &mut m.left,
        "right" => &mut m.right,
        "header" => &mut m.header,
        _ => &mut m.footer,
    }
}

/// The paper list the dialog offers: the Size menu's, and the sheet's own
/// code when it is not one of them.
fn papers(current: u32) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = PAPERS.iter().map(|&(c, n)| (c, n.to_string())).collect();
    if !out.iter().any(|&(c, _)| c == current) {
        out.push((current, format!("Paper size {current}")));
    }
    out
}

/// A number as a field shows it: `0.7`, not `0.70000`. The field is rounded,
/// so OK writes a margin only when its field was changed.
fn num(v: f64) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A control on tab `page`.
fn on(page: SetupTab, mut c: Control) -> Control {
    c.page = Some(page.index());
    c
}

fn text_control(
    page: SetupTab,
    name: &'static str,
    label: &str,
    kind: ControlKind,
    value: String,
) -> Control {
    on(page, Control::new(name, label, kind, Value::Text(value)))
}

fn check_control(page: SetupTab, name: &'static str, label: &str, value: bool) -> Control {
    on(
        page,
        Control::new(name, label, ControlKind::Checkbox, Value::Bool(value)),
    )
}

fn choice_control(
    page: SetupTab,
    name: &'static str,
    label: &str,
    kind: ControlKind,
    items: Vec<String>,
    at: Option<usize>,
) -> Control {
    let mut c = Control::new(name, label, kind, Value::Choice(at));
    c.items = items;
    on(page, c)
}

/// The Page Setup dialog for the active sheet, as it stands, open on `at`.
pub(crate) fn dialog(tab: &DocTab, at: SetupTab) -> Result<Dialog, String> {
    use SetupTab::{Margins as M, Page as P, Sheet as S};
    let Surface::Sheet(v) = &tab.surface else {
        return Err("page setup needs a spreadsheet".into());
    };
    let wb = &v.pkg.workbook;
    let s = v.active;
    let p = &wb.sheets[s].page_setup;
    let mut d = Dialog::message(
        "page-setup",
        "Page Setup",
        String::new(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::SheetPageSetup,
    );
    d.text = None;
    d.tabs = SetupTab::NAMES.iter().map(|t| t.to_string()).collect();
    d.tab = at.index();
    let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
    let list = papers(p.paper_size);
    // A blank page count is Automatic, as Excel's box reads it.
    let pages = |n: u32| if n == 0 { String::new() } else { n.to_string() };
    let mut controls = vec![
        choice_control(
            P,
            "orientation",
            "Orientation",
            ControlKind::Radio,
            strings(&ORIENTATIONS),
            Some(usize::from(p.orientation.is_landscape())),
        ),
        choice_control(
            P,
            "scaling",
            "Scaling",
            ControlKind::Radio,
            strings(&SCALING),
            Some(usize::from(p.fit_to_page)),
        ),
        text_control(
            P,
            "scale",
            "% normal size",
            ControlKind::Number,
            p.scale.to_string(),
        ),
        text_control(
            P,
            "fit-width",
            "page(s) wide by",
            ControlKind::Number,
            pages(p.fit_width),
        ),
        text_control(
            P,
            "fit-height",
            "tall",
            ControlKind::Number,
            pages(p.fit_height),
        ),
        choice_control(
            P,
            "paper",
            "Paper size:",
            ControlKind::Dropdown,
            list.iter().map(|(_, n)| n.clone()).collect(),
            list.iter().position(|&(c, _)| c == p.paper_size),
        ),
    ];
    for ((name, label), v) in MARGINS.iter().zip(margin_fields(&p.margins)) {
        controls.push(text_control(M, name, label, ControlKind::Number, num(v)));
    }
    controls.push(check_control(
        M,
        "h-centered",
        "Center horizontally",
        p.h_centered,
    ));
    controls.push(check_control(
        M,
        "v-centered",
        "Center vertically",
        p.v_centered,
    ));
    let print_area = area::print_area(wb, s)
        .into_iter()
        .map(area::rect_name)
        .collect::<Vec<_>>()
        .join(",");
    controls.push(text_control(
        S,
        "print-area",
        "Print area:",
        ControlKind::Text,
        print_area,
    ));
    let titles = area::print_titles(wb, s);
    let span = |v: Option<(u32, u32)>, rows: bool| match v {
        Some((a, b)) if rows => format!("${}:${}", a + 1, b + 1),
        Some((a, b)) => format!(
            "${}:${}",
            gridcore::sheet::col_name(a),
            gridcore::sheet::col_name(b)
        ),
        None => String::new(),
    };
    controls.push(text_control(
        S,
        "title-rows",
        "Rows to repeat at top:",
        ControlKind::Text,
        span(titles.rows, true),
    ));
    controls.push(text_control(
        S,
        "title-cols",
        "Columns to repeat at left:",
        ControlKind::Text,
        span(titles.cols, false),
    ));
    controls.push(check_control(S, "gridlines", "Gridlines", p.grid_lines));
    controls.push(check_control(
        S,
        "headings",
        "Row and column headings",
        p.headings,
    ));
    d.controls = controls;
    d.mark_opened();
    Ok(d)
}

fn control<'a>(d: &'a Dialog, name: &str) -> Option<&'a Control> {
    d.controls.iter().find(|c| c.name == name)
}

fn text(d: &Dialog, name: &str) -> String {
    control(d, name).map(|c| c.text()).unwrap_or_default()
}

fn choice(d: &Dialog, name: &str) -> Option<usize> {
    match control(d, name)?.value {
        Value::Choice(i) => i,
        _ => None,
    }
}

fn checked(d: &Dialog, name: &str) -> bool {
    control(d, name).is_some_and(|c| c.value == Value::Bool(true))
}

/// What the dialog's OK writes: the page setup, and the print area and
/// titles only when their fields were changed (`None` keeps the sheet's
/// definition, which the fields may not be able to show: `INDIRECT(...)`,
/// `#REF!`).
#[derive(Debug, PartialEq)]
struct Staged {
    setup: PageSetup,
    print_area: Option<Vec<area::Rect>>,
    titles: Option<PrintTitles>,
}

/// A whole number typed in `name`; blank reads as `blank`.
fn whole(d: &Dialog, name: &str, label: &str, blank: Option<u32>) -> Result<u32, String> {
    let t = text(d, name);
    let t = t.trim();
    match (t.is_empty(), blank) {
        (true, Some(b)) => Ok(b),
        _ => t
            .parse()
            .map_err(|_| format!("{label} must be a whole number, not '{t}'")),
    }
}

/// The references typed in `name`, each part one reference on `sheet`, or
/// why not.
fn refs(d: &Dialog, name: &str, label: &str, sheet: &str) -> Result<Vec<PrintRef>, String> {
    let t = text(d, name);
    let mut out = Vec::new();
    // Commas outside a quoted sheet name ('Sales, East'!A1) split it.
    for part in area::split_top_level(&t)
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if let Some(other) = area::split_sheet(part)
            .0
            .filter(|o| !o.eq_ignore_ascii_case(sheet))
        {
            return Err(format!(
                "{label} '{}' is on sheet '{other}', not on '{sheet}'",
                t.trim()
            ));
        }
        match area::parse_refs(part).as_slice() {
            [r] => out.push(*r),
            _ => return Err(format!("{label} '{}' is not a reference", t.trim())),
        }
    }
    Ok(out)
}

/// The rows or columns typed in a print-titles field: one span of whole
/// rows (`$1:$2`) or whole columns (`$A:$B`), or blank for none.
fn title_span(
    d: &Dialog,
    name: &str,
    rows: bool,
    sheet: &str,
) -> Result<Option<(u32, u32)>, String> {
    let label = if rows {
        "Rows to repeat at top"
    } else {
        "Columns to repeat at left"
    };
    match refs(d, name, label, sheet)?.as_slice() {
        [] => Ok(None),
        [PrintRef::Rows(a, b)] if rows => Ok(Some((*a, *b))),
        [PrintRef::Cols(a, b)] if !rows => Ok(Some((*a, *b))),
        _ => Err(format!(
            "{label} must be {} like {}, not '{}'",
            if rows { "whole rows" } else { "whole columns" },
            if rows { "$1:$2" } else { "$A:$B" },
            text(d, name).trim()
        )),
    }
}

/// Read and check the dialog against `now`, sheet `sheet`'s page setup. Only
/// a changed field is read: the dialog shows some values rounded (margins to
/// four places) or not at all (a definition its fields cannot spell), and an
/// untouched OK must leave them as they are.
fn staged(d: &Dialog, now: &PageSetup, sheet: &str) -> Result<Staged, String> {
    let changed = |name| d.changed(name);
    let mut p = now.clone();
    if changed("orientation") {
        p.orientation = match choice(d, "orientation") {
            Some(1) => Orientation::Landscape,
            _ => Orientation::Portrait,
        };
    }
    if changed("paper") {
        let list = papers(now.paper_size);
        if let Some(&(code, _)) = choice(d, "paper").and_then(|i| list.get(i)) {
            p.paper_size = code;
        }
    }
    if changed("scaling") {
        p.fit_to_page = choice(d, "scaling") == Some(1);
    }
    if changed("scale") {
        p.scale = whole(d, "scale", "Adjust to", None)?;
    }
    if changed("fit-width") {
        p.fit_width = whole(d, "fit-width", "Fit to page(s) wide", Some(0))?;
    }
    if changed("fit-height") {
        p.fit_height = whole(d, "fit-height", "Fit to pages tall", Some(0))?;
    }
    for (name, label) in MARGINS {
        if !changed(name) {
            continue;
        }
        let t = text(d, name);
        *margin_mut(&mut p.margins, name) = t.trim().parse().map_err(|_| {
            format!(
                "{} margin must be a number of inches, not '{}'",
                label.trim_end_matches(':'),
                t.trim()
            )
        })?;
    }
    for (name, field) in [
        ("h-centered", &mut p.h_centered),
        ("v-centered", &mut p.v_centered),
        ("gridlines", &mut p.grid_lines),
        ("headings", &mut p.headings),
    ] {
        if changed(name) {
            *field = checked(d, name);
        }
    }
    p.validate()?;
    let print_area = if changed("print-area") {
        Some(
            refs(d, "print-area", "Print area", sheet)?
                .into_iter()
                .map(PrintRef::rect)
                .collect(),
        )
    } else {
        None
    };
    let titles = if changed("title-rows") || changed("title-cols") {
        Some(PrintTitles {
            rows: title_span(d, "title-rows", true, sheet)?,
            cols: title_span(d, "title-cols", false, sheet)?,
        })
    } else {
        None
    };
    Ok(Staged {
        setup: p,
        print_area,
        titles,
    })
}

/// A press the Page Setup dialog handles itself: OK applies it as one undo
/// step and closes it; a refused value keeps it open and changes nothing.
/// `None` for any other dialog or button (Cancel closes through the stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    if top.owner != DialogOwner::SheetPageSetup
        || !button.replace('&', "").trim().eq_ignore_ascii_case("OK")
    {
        return None;
    }
    let Surface::Sheet(v) = &tab.surface else {
        return Some(Err("page setup needs a spreadsheet".into()));
    };
    let sheet = &v.pkg.workbook.sheets[v.active];
    let staged = match staged(top, &sheet.page_setup, &sheet.name) {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let outcome = edit(tab, |v, s| {
        let wb = &mut v.pkg.workbook;
        wb.sheets[s].page_setup = staged.setup;
        if let Some(rects) = &staged.print_area {
            area::set_print_area(wb, s, rects);
        }
        if let Some(titles) = staged.titles {
            area::set_print_titles(wb, s, titles);
        }
        Ok(())
    });
    tab.dialogs.pop();
    tab.status = match outcome {
        Ok(true) => "Page setup changed".into(),
        Ok(false) => "Nothing to change".into(),
        Err(e) => e.into(),
    };
    Some(Ok(()))
}

#[cfg(test)]
#[path = "sheet_page_setup_tests.rs"]
mod tests;
