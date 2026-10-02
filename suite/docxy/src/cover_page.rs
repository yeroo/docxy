//! Insert › Pages (#652): the Cover Page menu and Blank Page, beside Page
//! Break, in Word's order.
//!
//! Cover Page's gallery puts one of [`COVER_DESIGNS`] at the start of the
//! document, or in place of the cover it has, keeping text typed into the
//! placeholders; Remove Current Cover Page deletes it (see
//! [`docxcore::cover`]). Both act on the body editor, one undo step each.
//! Blank Page inserts two page breaks at the body caret.
//!
//! Neither runs while a header or footer is being edited (Word disables
//! them there), and Cover Page needs a .docx: Markdown keeps no content
//! controls or section properties.
use super::*;
use docxcore::cover::{COVER_DESIGNS, sdt_ids};

/// A Cover Page menu command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CoverAct {
    /// The drop-down button: open the menu.
    Menu,
    /// A gallery design, by its index in [`COVER_DESIGNS`].
    Design(usize),
    Remove,
}

/// The Cover Page button's id.
pub(crate) const MENU_ID: &str = "cover-page";

const DESIGN_IDS: [&str; 4] = ["cover-plain", "cover-centered", "cover-band", "cover-ruled"];

fn cover(act: CoverAct) -> Act {
    Act::Cover(act)
}

/// The drop-down's commands for `ribbon-read` and `ribbon-click`: the
/// designs, then Remove Current Cover Page.
pub(crate) fn ribbon_items() -> Vec<rs::Cmd<Act>> {
    let mut out: Vec<rs::Cmd<Act>> = COVER_DESIGNS
        .iter()
        .enumerate()
        .map(|(i, d)| {
            rs::cmd(
                DESIGN_IDS[i],
                "cover-page",
                d.name,
                cover(CoverAct::Design(i)),
            )
        })
        .collect();
    out.push(rs::cmd(
        "cover-remove",
        "cover-page",
        "Remove Current Cover Page",
        cover(CoverAct::Remove),
    ));
    out
}

/// The Insert tab's Pages group: Cover Page, Blank Page, Page Break.
pub(crate) fn pages_group() -> Vec<Control<Act>> {
    vec![
        Control::Dropdown {
            cmd: cmdt(
                MENU_ID,
                "cover-page",
                "Cover Page",
                cover(CoverAct::Menu),
                "",
            )
            .tip(
                "Cover Page",
                "Start the document with a cover page, or replace or remove it.",
                "",
            )
            .key("V"),
            items: ribbon_items(),
        },
        Control::Large(
            cmdt("blankpage", "blank-page", "Blank Page", Act::BlankPage, "")
                .tip("Blank Page", "Add a new blank page at the cursor.", "")
                .key("NP"),
        ),
        Control::Large(cmdt("pagebreak", "rule", "Page Break", Act::PageBreak, "").key("B")),
    ]
}

/// Whether the tab's document has a cover page.
pub(crate) fn has_cover(tab: Option<&DocTab>) -> bool {
    matches!(tab.map(|t| &t.surface), Some(Surface::Doc(ed)) if ed.has_cover_page())
}

/// The Cover Page menu: Word's Built-in heading over the designs, then
/// Remove Current Cover Page, available when there is a cover.
pub(crate) fn menu_items(tab: Option<&DocTab>) -> Vec<menu::MenuItem> {
    use menu::{Entry, MenuItem};
    let mut items = vec![MenuItem::Heading("Built-in".into())];
    items.extend(COVER_DESIGNS.iter().enumerate().map(|(i, d)| {
        MenuItem::Item(Entry::new(
            DESIGN_IDS[i],
            d.name,
            "cover-page",
            cover(CoverAct::Design(i)),
            true,
        ))
    }));
    items.push(MenuItem::Separator);
    items.push(MenuItem::Item(Entry::new(
        "cover-remove",
        "Remove Current Cover Page",
        "",
        cover(CoverAct::Remove),
        has_cover(tab),
    )));
    items
}

/// Why the body editor of `tab` cannot take a Pages command now, if it
/// cannot: no document, or a header or footer being edited.
fn body_refusal(tab: Option<&DocTab>) -> Option<&'static str> {
    match tab {
        Some(t) if t.hf_edit.is_some() => Some("Close the header or footer first"),
        Some(t) if matches!(t.surface, Surface::Doc(_)) => None,
        _ => Some("Pages commands need a document"),
    }
}

/// Why Cover Page cannot act on `tab` now, if it cannot.
fn cover_refusal(tab: Option<&DocTab>) -> Option<&'static str> {
    body_refusal(tab).or_else(|| {
        tab.is_some_and(|t| t.markdown)
            .then_some("A cover page needs a .docx (not a Markdown document)")
    })
}

/// Whether a Cover Page command can run: a .docx body being edited, and for
/// Remove, a cover to remove.
pub(crate) fn cover_enabled(tab: Option<&DocTab>, act: CoverAct) -> bool {
    cover_refusal(tab).is_none() && (act != CoverAct::Remove || has_cover(tab))
}

/// Whether Blank Page can run: a document body being edited.
pub(crate) fn blank_page_enabled(tab: Option<&DocTab>) -> bool {
    body_refusal(tab).is_none()
}

/// The content-control ids the package's headers and footers hold, which a
/// new cover's controls must not reuse.
fn part_sdt_ids(pkg: Option<&Package>) -> Vec<i64> {
    let Some(pkg) = pkg else {
        return Vec::new();
    };
    pkg.part_names()
        .into_iter()
        .filter(|n| n.starts_with("word/header") || n.starts_with("word/footer"))
        .filter_map(|n| pkg.part_text(n))
        .flat_map(|xml| sdt_ids(&xml))
        .collect()
}

/// Run a Cover Page command on the tab's body, with its status line.
pub(crate) fn cover_apply(tab: &mut DocTab, act: CoverAct) -> Result<(), String> {
    if let Some(why) = cover_refusal(Some(tab)) {
        return Err(why.into());
    }
    let used = part_sdt_ids(tab.pkg.as_ref());
    let Surface::Doc(ed) = &mut tab.surface else {
        return Err("Pages commands need a document".into());
    };
    match act {
        CoverAct::Menu => return Ok(()),
        CoverAct::Design(i) => {
            ed.set_cover_page(i, &used)?;
            tab.status = format!("Cover page: {}", COVER_DESIGNS[i].name).into();
        }
        CoverAct::Remove => {
            if !ed.remove_cover_page() {
                return Err("This document has no cover page".into());
            }
            tab.status = "Cover page removed".into();
        }
    }
    tab.dirty = true;
    Ok(())
}

/// Blank Page: two page breaks at the body caret.
pub(crate) fn blank_page_apply(tab: &mut DocTab) -> Result<(), String> {
    if let Some(why) = body_refusal(Some(tab)) {
        return Err(why.into());
    }
    if let Surface::Doc(ed) = &mut tab.surface {
        ed.insert_blank_page();
        tab.dirty = true;
    }
    Ok(())
}

impl Docxy {
    /// Dispatch a Cover Page command.
    pub(crate) fn cover_act(&mut self, act: CoverAct, window: &mut Window, cx: &mut Context<Self>) {
        if act == CoverAct::Menu {
            let at = split_menu_anchor(&self.probes.borrow(), MENU_ID)
                .unwrap_or_else(|| point(px(40.), px(140.)));
            if let Err(e) = self.open_split_menu(MENU_ID, at, cx) {
                self.set_status(e);
            }
            return;
        }
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Err(e) = cover_apply(tab, act) {
                tab.status = e.into();
            }
        }
        self.refocus(window, cx);
    }

    /// Insert › Blank Page.
    pub(crate) fn insert_blank_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Err(e) = blank_page_apply(tab) {
                tab.status = e.into();
            }
        }
        self.refocus(window, cx);
    }
}

#[cfg(test)]
mod tests;
