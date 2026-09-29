//! The Word ribbon's Layout tab (#649): the Page Setup group's seven menus,
//! each command acting on the sections the caret or selection touches.
//!
//! Each menu is a [`Control::Dropdown`]. Its button opens the menu (the
//! pointer, a KeyTip, or `menu-open`), and the menu is built here from one
//! spec per menu, with Word's headings and separators; the dropdown's `items`
//! carry the same commands for `ribbon-read` and the harness.
//!
//! Section edits go through the body editor's [`Editor::edit_section_setups`]
//! as one undo step, even while a header or footer is open: the body caret
//! picks the section. They are untracked, as the Insert tab's old Columns
//! toggle was. Settings edits (hyphenation, mirrored margins) write the
//! package and have no undo, as before.
use super::*;
use docxcore::model::BreakKind;
use docxcore::sect::{LineNumbering, LnRestart, Paper, SectionSetup, SectionStart};

/// A Layout tab command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LayoutAct {
    /// A Page Setup group button: open its menu.
    Menu(LayoutMenu),
    Margins(MarginPreset),
    /// Portrait (`false`) or Landscape (`true`).
    Orient(bool),
    Paper(Paper),
    Columns(ColumnsPreset),
    Break(BreakChoice),
    LineNumbers(LnChoice),
    SuppressLineNumbers,
    /// Automatic hyphenation on or off.
    Hyphen(bool),
    /// Open the Page Setup dialog on a tab.
    PageSetup(PageSetupTab),
    /// Open the Columns dialog.
    MoreColumns,
    /// An item Word has that has nothing behind it yet: drawn disabled.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LayoutMenu {
    Margins,
    Orientation,
    Size,
    Columns,
    Breaks,
    LineNumbers,
    Hyphenation,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MarginPreset {
    Normal,
    Narrow,
    Moderate,
    Wide,
    Mirrored,
}

impl MarginPreset {
    /// (top, bottom, left or inside, right or outside), in twips.
    fn values(self) -> (i32, i32, i32, i32) {
        match self {
            Self::Normal => (1440, 1440, 1440, 1440),
            Self::Narrow => (720, 720, 720, 720),
            Self::Moderate => (1440, 1440, 1080, 1080),
            Self::Wide => (1440, 1440, 2880, 2880),
            Self::Mirrored => (1440, 1440, 1800, 1440),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ColumnsPreset {
    One,
    Two,
    Three,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BreakChoice {
    Page,
    Column,
    TextWrapping,
    Section(SectionStart),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LnChoice {
    None,
    Continuous,
    RestartEachPage,
    RestartEachSection,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PageSetupTab {
    Margins,
    Paper,
}

/// One row of a Layout menu.
enum Row {
    Item {
        id: &'static str,
        label: &'static str,
        act: LayoutAct,
    },
    Heading(&'static str),
    Separator,
}

fn item(id: &'static str, label: &'static str, act: LayoutAct) -> Row {
    Row::Item { id, label, act }
}

/// Every menu's rows, in Word's order.
fn menu_rows(menu: LayoutMenu) -> Vec<Row> {
    use LayoutAct as L;
    match menu {
        LayoutMenu::Margins => vec![
            item("margins-normal", "Normal", L::Margins(MarginPreset::Normal)),
            item("margins-narrow", "Narrow", L::Margins(MarginPreset::Narrow)),
            item(
                "margins-moderate",
                "Moderate",
                L::Margins(MarginPreset::Moderate),
            ),
            item("margins-wide", "Wide", L::Margins(MarginPreset::Wide)),
            item(
                "margins-mirrored",
                "Mirrored",
                L::Margins(MarginPreset::Mirrored),
            ),
            Row::Separator,
            item(
                "margins-custom",
                "Custom Margins...",
                L::PageSetup(PageSetupTab::Margins),
            ),
        ],
        LayoutMenu::Orientation => vec![
            item("orient-portrait", "Portrait", L::Orient(false)),
            item("orient-landscape", "Landscape", L::Orient(true)),
        ],
        LayoutMenu::Size => {
            let mut rows: Vec<Row> = Paper::ALL
                .into_iter()
                .map(|p| item(paper_id(p), p.label(), L::Paper(p)))
                .collect();
            rows.push(Row::Separator);
            rows.push(item(
                "size-more",
                "More Paper Sizes...",
                L::PageSetup(PageSetupTab::Paper),
            ));
            rows
        }
        LayoutMenu::Columns => vec![
            item("columns-one", "One", L::Columns(ColumnsPreset::One)),
            item("columns-two", "Two", L::Columns(ColumnsPreset::Two)),
            item("columns-three", "Three", L::Columns(ColumnsPreset::Three)),
            item("columns-left", "Left", L::Columns(ColumnsPreset::Left)),
            item("columns-right", "Right", L::Columns(ColumnsPreset::Right)),
            Row::Separator,
            item("columns-more", "More Columns...", L::MoreColumns),
        ],
        LayoutMenu::Breaks => vec![
            Row::Heading("Page Breaks"),
            item("break-page", "Page", L::Break(BreakChoice::Page)),
            item("break-column", "Column", L::Break(BreakChoice::Column)),
            item(
                "break-wrapping",
                "Text Wrapping",
                L::Break(BreakChoice::TextWrapping),
            ),
            Row::Heading("Section Breaks"),
            item(
                "break-next",
                "Next Page",
                L::Break(BreakChoice::Section(SectionStart::NextPage)),
            ),
            item(
                "break-continuous",
                "Continuous",
                L::Break(BreakChoice::Section(SectionStart::Continuous)),
            ),
            item(
                "break-even",
                "Even Page",
                L::Break(BreakChoice::Section(SectionStart::EvenPage)),
            ),
            item(
                "break-odd",
                "Odd Page",
                L::Break(BreakChoice::Section(SectionStart::OddPage)),
            ),
        ],
        LayoutMenu::LineNumbers => vec![
            item("ln-none", "None", L::LineNumbers(LnChoice::None)),
            item(
                "ln-continuous",
                "Continuous",
                L::LineNumbers(LnChoice::Continuous),
            ),
            item(
                "ln-page",
                "Restart Each Page",
                L::LineNumbers(LnChoice::RestartEachPage),
            ),
            item(
                "ln-section",
                "Restart Each Section",
                L::LineNumbers(LnChoice::RestartEachSection),
            ),
            Row::Separator,
            item(
                "ln-suppress",
                "Suppress for Current Paragraph",
                L::SuppressLineNumbers,
            ),
            Row::Separator,
            item("ln-options", "Line Numbering Options...", L::Unavailable),
        ],
        LayoutMenu::Hyphenation => vec![
            item("hyphen-none", "None", L::Hyphen(false)),
            item("hyphen-auto", "Automatic", L::Hyphen(true)),
            item("hyphen-manual", "Manual", L::Unavailable),
            Row::Separator,
            item("hyphen-options", "Hyphenation Options...", L::Unavailable),
        ],
    }
}

fn paper_id(p: Paper) -> &'static str {
    match p {
        Paper::Letter => "size-letter",
        Paper::Legal => "size-legal",
        Paper::Executive => "size-executive",
        Paper::A4 => "size-a4",
        Paper::A5 => "size-a5",
        Paper::B5Jis => "size-b5",
        Paper::Tabloid => "size-tabloid",
    }
}

/// The group's buttons: (menu, id, icon, label, KeyTip).
const MENUS: [(LayoutMenu, &str, &str, &str, &str); 7] = [
    (
        LayoutMenu::Margins,
        "margins",
        "print-layout",
        "Margins",
        "M",
    ),
    (
        LayoutMenu::Orientation,
        "orientation",
        "new",
        "Orientation",
        "O",
    ),
    (LayoutMenu::Size, "size", "print-layout", "Size", "SZ"),
    (LayoutMenu::Columns, "columns", "columns", "Columns", "J"),
    (LayoutMenu::Breaks, "breaks", "rule", "Breaks", "B"),
    (
        LayoutMenu::LineNumbers,
        "linenumbers",
        "list-numbered",
        "Line Numbers",
        "LN",
    ),
    (
        LayoutMenu::Hyphenation,
        "hyphenation",
        "hyphenation",
        "Hyphenation",
        "H",
    ),
];

/// The Layout ribbon tab.
pub(crate) fn layout_tab() -> rs::Tab<Act> {
    let controls = MENUS
        .iter()
        .map(|&(menu, id, icon, label, key)| Control::Dropdown {
            cmd: cmdt(id, icon, label, Act::Layout(LayoutAct::Menu(menu)), "").key(key),
            items: menu_rows(menu)
                .into_iter()
                .filter_map(|row| match row {
                    Row::Item { id, label, act } => {
                        Some(rs::cmd(id, icon, label, Act::Layout(act)))
                    }
                    _ => None,
                })
                .collect(),
        })
        .collect();
    rs::tab(
        "Layout",
        "P",
        vec![
            rs::group("Page Setup", 40, controls)
                .launcher(Act::Layout(LayoutAct::PageSetup(PageSetupTab::Margins))),
        ],
    )
}

/// The menu a Page Setup button opens, by the button's id.
pub(crate) fn menu_of(id: &str) -> Option<LayoutMenu> {
    MENUS.iter().find(|m| m.1 == id).map(|m| m.0)
}

fn menu_id(menu: LayoutMenu) -> &'static str {
    MENUS.iter().find(|m| m.0 == menu).map_or("", |m| m.1)
}

/// A Layout menu's items, with the live checked states.
pub(crate) fn menu_items(menu: LayoutMenu, checked: impl Fn(Act) -> bool) -> Vec<menu::MenuItem> {
    menu_rows(menu)
        .into_iter()
        .map(|row| match row {
            Row::Item { id, label, act } => {
                let entry = if act == LayoutAct::Unavailable {
                    menu::Entry::unavailable(id, label)
                } else {
                    menu::Entry::new(id, label, "", Act::Layout(act), true)
                        .checked(checked(Act::Layout(act)))
                };
                menu::MenuItem::Item(entry)
            }
            Row::Heading(h) => menu::MenuItem::Heading(h.into()),
            Row::Separator => menu::MenuItem::Separator,
        })
        .collect()
}

/// Whether a Layout command can run: the placeholders cannot.
pub(crate) fn layout_enabled(act: LayoutAct) -> bool {
    act != LayoutAct::Unavailable
}

/// The body editor and package of a .docx tab, or why a Layout command has
/// nothing to act on.
fn body(tab: &mut DocTab) -> Result<(&mut Editor, &mut Package), String> {
    match (&mut tab.surface, tab.pkg.as_mut()) {
        (Surface::Doc(ed), Some(pkg)) => Ok((ed, pkg)),
        (Surface::Doc(_), None) => Err("Page layout needs a .docx (not Markdown)".into()),
        _ => Err("Page layout needs a document".into()),
    }
}

/// Whether a Layout command's choice is the current one, for the check mark.
pub(crate) fn layout_checked(tab: &DocTab, act: LayoutAct) -> bool {
    let (Surface::Doc(ed), Some(pkg)) = (&tab.surface, tab.pkg.as_ref()) else {
        return false;
    };
    let sections = ed.sections();
    let setup = SectionSetup::parse(&sections[ed.caret_section().min(sections.len() - 1)]);
    let m = setup.margins;
    match act {
        LayoutAct::Margins(p) => {
            let (top, bottom, left, right) = p.values();
            (p == MarginPreset::Mirrored) == pkg.has_mirror_margins()
                && (m.top, m.bottom, m.left, m.right) == (top, bottom, left, right)
        }
        LayoutAct::Orient(landscape) => setup.page.landscape == landscape,
        LayoutAct::Paper(p) => Paper::matching(setup.page.w, setup.page.h) == Some(p),
        LayoutAct::Columns(c) => {
            let cols = &setup.columns;
            match c {
                ColumnsPreset::One => cols.count() == 1,
                ColumnsPreset::Two => cols.equal_width() && cols.count() == 2,
                ColumnsPreset::Three => cols.equal_width() && cols.count() == 3,
                ColumnsPreset::Left => cols.cols.len() == 2 && cols.cols[0].w < cols.cols[1].w,
                ColumnsPreset::Right => cols.cols.len() == 2 && cols.cols[0].w > cols.cols[1].w,
            }
        }
        LayoutAct::LineNumbers(c) => match (c, setup.line_numbers) {
            (LnChoice::None, None) => true,
            (LnChoice::Continuous, Some(l)) => l.restart == LnRestart::Continuous,
            (LnChoice::RestartEachPage, Some(l)) => l.restart == LnRestart::NewPage,
            (LnChoice::RestartEachSection, Some(l)) => l.restart == LnRestart::NewSection,
            _ => false,
        },
        LayoutAct::SuppressLineNumbers => ed.caret_suppresses_line_numbers(),
        LayoutAct::Hyphen(on) => pkg.has_auto_hyphenation() == on,
        _ => false,
    }
}

/// Run a Layout command that edits the document (every one but the menus and
/// dialogs) on a tab. The status line says what happened; an error says why
/// nothing did.
pub(crate) fn layout_apply(tab: &mut DocTab, act: LayoutAct) -> Result<(), String> {
    let status: String = match act {
        LayoutAct::Margins(p) => {
            let (ed, pkg) = body(tab)?;
            let (top, bottom, left, right) = p.values();
            let k = ed.target_sections();
            ed.edit_section_setups(&k, |s| {
                (
                    s.margins.top,
                    s.margins.bottom,
                    s.margins.left,
                    s.margins.right,
                ) = (top, bottom, left, right);
            });
            pkg.set_mirror_margins(p == MarginPreset::Mirrored);
            format!("Margins: {p:?}")
        }
        LayoutAct::Orient(landscape) => {
            let (ed, _) = body(tab)?;
            let k = ed.target_sections();
            ed.edit_section_setups(&k, |s| s.set_landscape(landscape));
            if landscape {
                "Orientation: Landscape"
            } else {
                "Orientation: Portrait"
            }
            .into()
        }
        LayoutAct::Paper(p) => {
            let (ed, _) = body(tab)?;
            let k = ed.target_sections();
            ed.edit_section_setups(&k, |s| s.page.set_paper(p));
            format!("Size: {}", p.label())
        }
        LayoutAct::Columns(c) => {
            let (ed, pkg) = body(tab)?;
            let gutter_at_top = pkg.has_gutter_at_top();
            let k = ed.target_sections();
            ed.edit_section_setups(&k, |s| {
                s.columns = match c {
                    ColumnsPreset::One => s.columns.equal(1, 720),
                    ColumnsPreset::Two => s.columns.equal(2, 720),
                    ColumnsPreset::Three => s.columns.equal(3, 720),
                    ColumnsPreset::Left => s.columns.two_unequal(s.text_width(gutter_at_top), true),
                    ColumnsPreset::Right => {
                        s.columns.two_unequal(s.text_width(gutter_at_top), false)
                    }
                }
            });
            format!("Columns: {c:?}")
        }
        LayoutAct::LineNumbers(c) => {
            let (ed, _) = body(tab)?;
            let k = ed.target_sections();
            ed.edit_section_setups(&k, |s| {
                let restart = match c {
                    LnChoice::None => {
                        s.line_numbers = None;
                        return;
                    }
                    LnChoice::Continuous => LnRestart::Continuous,
                    LnChoice::RestartEachPage => LnRestart::NewPage,
                    LnChoice::RestartEachSection => LnRestart::NewSection,
                };
                let old = s.line_numbers;
                s.line_numbers = Some(LineNumbering {
                    count_by: 1,
                    start: old.and_then(|l| l.start),
                    distance: old.and_then(|l| l.distance),
                    restart,
                });
            });
            format!("Line numbers: {c:?}")
        }
        LayoutAct::SuppressLineNumbers => {
            let (ed, _) = body(tab)?;
            ed.toggle_suppress_line_numbers();
            "Suppress line numbers toggled".into()
        }
        LayoutAct::Hyphen(on) => {
            let (_, pkg) = body(tab)?;
            pkg.set_auto_hyphenation(on);
            if on {
                "Automatic hyphenation: on".into()
            } else {
                "Automatic hyphenation: off".into()
            }
        }
        LayoutAct::Break(BreakChoice::Section(start)) => {
            if tab.hf_edit.is_some() {
                return Err("A section break cannot go in a header or footer".into());
            }
            let (ed, _) = body(tab)?;
            ed.insert_section_break(start)?;
            format!("Section break ({})", start.val())
        }
        LayoutAct::Break(choice) => {
            let kind = match choice {
                BreakChoice::Page => BreakKind::Page,
                BreakChoice::Column => BreakKind::Column,
                _ => BreakKind::Clear(docxcore::model::ClearKind::All),
            };
            // A page, column or wrapping break goes where the caret is typing:
            // the open header or footer, else the body.
            match tab.hf_edit.as_mut() {
                Some(hf) => hf.editor.insert_break(kind),
                None => body(tab)?.0.insert_break(kind),
            }
            format!("{choice:?} break")
        }
        LayoutAct::Menu(_)
        | LayoutAct::PageSetup(_)
        | LayoutAct::MoreColumns
        | LayoutAct::Unavailable => return Ok(()),
    };
    tab.dirty = true;
    tab.status = status.into();
    Ok(())
}

impl Docxy {
    /// Dispatch a Layout tab command.
    pub(crate) fn layout_act(
        &mut self,
        act: LayoutAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match act {
            LayoutAct::Menu(menu) => {
                let id = menu_id(menu);
                let at = split_menu_anchor(&self.probes.borrow(), id)
                    .unwrap_or_else(|| point(px(0.), px(0.)));
                if let Err(e) = self.open_split_menu(id, at, cx) {
                    self.set_status(e);
                }
                return;
            }
            LayoutAct::PageSetup(_) | LayoutAct::MoreColumns | LayoutAct::Unavailable => {
                self.set_status("This dialog is not available yet");
            }
            _ => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if let Err(e) = layout_apply(tab, act) {
                        tab.status = e.into();
                    }
                }
            }
        }
        self.refocus(window, cx);
    }
}

#[cfg(test)]
mod tests;
