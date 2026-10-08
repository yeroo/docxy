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
    /// The Page Setup dialog: Custom Margins..., More Paper Sizes..., Print
    /// Titles, More Pages..., Custom... and the groups' launchers.
    Dialog,
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
        PageAct::Area(_) | PageAct::Break(_) | PageAct::Dialog => false,
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

/// Apply one command to `v`'s sheet `s`.
fn apply(v: &mut SheetView, s: usize, act: PageAct) {
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
                PageAct::Area(_) | PageAct::Break(_) | PageAct::Dialog => {}
            }
        }
    }
}

/// Run a Page Layout command on the tab.
pub(crate) fn run(tab: &mut DocTab, act: PageAct) {
    if act == PageAct::Dialog {
        match dialog(tab) {
            Ok(d) => tab.dialogs.push(d),
            Err(e) => tab.status = e.into(),
        }
        return;
    }
    if let Err(e) = edit(tab, |v, s| {
        apply(v, s, act);
        Ok(())
    }) {
        tab.status = e.into();
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

/// The paper list the dialog offers: the Size menu's, and the sheet's own
/// code when it is not one of them.
fn papers(current: u32) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = PAPERS.iter().map(|&(c, n)| (c, n.to_string())).collect();
    if !out.iter().any(|&(c, _)| c == current) {
        out.push((current, format!("Paper size {current}")));
    }
    out
}

/// A number as a field shows it: `0.7`, not `0.70000`.
fn num(v: f64) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn text_control(name: &'static str, label: &str, kind: ControlKind, value: String) -> Control {
    Control::new(name, label, kind, Value::Text(value))
}

/// The Page Setup dialog for the active sheet, as it stands.
pub(crate) fn dialog(tab: &DocTab) -> Result<Dialog, String> {
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
    let mut controls = Vec::new();
    let mut orientation = Control::new(
        "orientation",
        "Orientation",
        ControlKind::Radio,
        Value::Choice(Some(usize::from(p.orientation.is_landscape()))),
    );
    orientation.items = ORIENTATIONS.iter().map(|s| s.to_string()).collect();
    controls.push(orientation);
    let list = papers(p.paper_size);
    let mut paper = Control::new(
        "paper",
        "Paper size:",
        ControlKind::Dropdown,
        Value::Choice(list.iter().position(|&(c, _)| c == p.paper_size)),
    );
    paper.items = list.into_iter().map(|(_, n)| n).collect();
    controls.push(paper);
    let mut scaling = Control::new(
        "scaling",
        "Scaling",
        ControlKind::Radio,
        Value::Choice(Some(usize::from(p.fit_to_page))),
    );
    scaling.items = SCALING.iter().map(|s| s.to_string()).collect();
    controls.push(scaling);
    controls.push(text_control(
        "scale",
        "% normal size",
        ControlKind::Number,
        p.scale.to_string(),
    ));
    // A blank page count is Automatic, as Excel's box reads it.
    let pages = |n: u32| if n == 0 { String::new() } else { n.to_string() };
    controls.push(text_control(
        "fit-width",
        "page(s) wide by",
        ControlKind::Number,
        pages(p.fit_width),
    ));
    controls.push(text_control(
        "fit-height",
        "tall",
        ControlKind::Number,
        pages(p.fit_height),
    ));
    for ((name, label), v) in MARGINS.iter().zip(margin_fields(&p.margins)) {
        controls.push(text_control(name, label, ControlKind::Number, num(v)));
    }
    for (name, label, on) in [
        ("h-centered", "Center horizontally", p.h_centered),
        ("v-centered", "Center vertically", p.v_centered),
    ] {
        controls.push(Control::new(
            name,
            label,
            ControlKind::Checkbox,
            Value::Bool(on),
        ));
    }
    let print_area = area::print_area(wb, s)
        .into_iter()
        .map(area::rect_name)
        .collect::<Vec<_>>()
        .join(",");
    controls.push(text_control(
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
        "title-rows",
        "Rows to repeat at top:",
        ControlKind::Text,
        span(titles.rows, true),
    ));
    controls.push(text_control(
        "title-cols",
        "Columns to repeat at left:",
        ControlKind::Text,
        span(titles.cols, false),
    ));
    for (name, label, on) in [
        ("gridlines", "Gridlines", p.grid_lines),
        ("headings", "Row and column headings", p.headings),
    ] {
        controls.push(Control::new(
            name,
            label,
            ControlKind::Checkbox,
            Value::Bool(on),
        ));
    }
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

/// What the dialog's OK writes: the page setup, the print area and titles.
#[derive(Debug, PartialEq)]
struct Staged {
    setup: PageSetup,
    print_area: Vec<area::Rect>,
    titles: PrintTitles,
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

/// The references typed in `name`, each part one reference, or why not.
fn refs(d: &Dialog, name: &str, label: &str) -> Result<Vec<PrintRef>, String> {
    let t = text(d, name);
    let parts = t.split(',').filter(|p| !p.trim().is_empty()).count();
    let refs = area::parse_refs(&t);
    if parts == 0 {
        return Ok(Vec::new());
    }
    if refs.len() != parts {
        return Err(format!("{label} '{}' is not a reference", t.trim()));
    }
    Ok(refs)
}

/// The rows or columns typed in a print-titles field: one span of whole
/// rows (`$1:$2`) or whole columns (`$A:$B`), or blank for none.
fn title_span(d: &Dialog, name: &str, rows: bool) -> Result<Option<(u32, u32)>, String> {
    let label = if rows {
        "Rows to repeat at top"
    } else {
        "Columns to repeat at left"
    };
    match refs(d, name, label)?.as_slice() {
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

/// Read and check the dialog's values against `now`, the sheet's page setup.
fn staged(d: &Dialog, now: &PageSetup) -> Result<Staged, String> {
    let mut p = now.clone();
    p.orientation = match choice(d, "orientation") {
        Some(1) => Orientation::Landscape,
        // Unchanged from `default` keeps `default`: it prints portrait.
        _ if !now.orientation.is_landscape() => now.orientation,
        _ => Orientation::Portrait,
    };
    if let Some(i) = choice(d, "paper") {
        if let Some(&(code, _)) = papers(now.paper_size).get(i) {
            p.paper_size = code;
        }
    }
    p.scale = whole(d, "scale", "Adjust to", None)?;
    p.fit_to_page = choice(d, "scaling") == Some(1);
    p.fit_width = whole(d, "fit-width", "Fit to page(s) wide", Some(0))?;
    p.fit_height = whole(d, "fit-height", "Fit to pages tall", Some(0))?;
    let mut m = [0.0; 6];
    for (slot, (name, label)) in m.iter_mut().zip(MARGINS) {
        let t = text(d, name);
        *slot = t.trim().parse().map_err(|_| {
            format!(
                "{} margin must be a number of inches, not '{}'",
                label.trim_end_matches(':'),
                t.trim()
            )
        })?;
    }
    [
        p.margins.top,
        p.margins.bottom,
        p.margins.left,
        p.margins.right,
        p.margins.header,
        p.margins.footer,
    ] = m;
    p.h_centered = checked(d, "h-centered");
    p.v_centered = checked(d, "v-centered");
    p.grid_lines = checked(d, "gridlines");
    p.headings = checked(d, "headings");
    p.validate()?;
    let print_area = refs(d, "print-area", "Print area")?
        .into_iter()
        .map(PrintRef::rect)
        .collect();
    let titles = PrintTitles {
        rows: title_span(d, "title-rows", true)?,
        cols: title_span(d, "title-cols", false)?,
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
    let now = &v.pkg.workbook.sheets[v.active].page_setup;
    let staged = match staged(top, now) {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let outcome = edit(tab, |v, s| {
        let wb = &mut v.pkg.workbook;
        wb.sheets[s].page_setup = staged.setup;
        area::set_print_area(wb, s, &staged.print_area);
        area::set_print_titles(wb, s, staged.titles);
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
