//! Word's contextual Header & Footer tab (#641), shown while a header or
//! footer is being edited, and the Insert tab's Header and Footer menus.
//!
//! Groups, in Word's order: Header & Footer (the Header, Footer and Page
//! Number menus), Insert, Navigation (Go to Header/Footer, Previous, Next,
//! Link to Previous), Options (Different First Page, Different Odd & Even
//! Pages, Show Document Text), Position (Header from Top, Footer from Bottom)
//! and Close.
//!
//! Every command acts on the section and variant being edited
//! ([`HfEdit::section`]), else the body caret's section. Section edits (link
//! changes, created references, titlePg, distances) are one step on the body
//! editor's undo stack; while a header is open, Ctrl+Z undoes typing in it,
//! and the body's steps undo after leaving it. Part contents (a design, a
//! removal, a copy made by unlinking) live in the package and are not undone;
//! an unreferenced part left behind is harmless.
use super::*;
use crate::dialog::{
    Button, ButtonRole, Control as Field, ControlKind, Dialog, DialogOwner, Value,
};
use crate::hf::PageSlot;
use ctlcore::json::Json;
use docxcore::sect::{TWIPS_PER_INCH, hf_reference, set_hf_reference};

/// A Header & Footer command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HfAct {
    /// A drop-down button or position box: open its menu.
    Menu(HfMenu),
    /// A built-in design from the Header (`is_header`) or Footer gallery.
    Design {
        is_header: bool,
        index: usize,
    },
    /// Edit Header / Edit Footer.
    Edit(bool),
    /// Remove Header / Remove Footer: empty the part, keep the reference.
    Remove(bool),
    /// Go to Header / Go to Footer.
    GoTo(bool),
    Previous,
    Next,
    LinkToPrevious,
    DifferentFirst,
    DifferentOddEven,
    ShowText,
    /// Header from Top (`is_header`) or Footer from Bottom, in twips.
    Distance {
        is_header: bool,
        twips: i32,
    },
    /// The Header from Top / Footer from Bottom box's Custom... dialog.
    CustomDistance(bool),
    Close,
    /// A Page Number menu command (#650).
    PageNumber(crate::page_number::PnAct),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HfMenu {
    Header,
    Footer,
    PageNumber,
    HeaderFromTop,
    FooterFromBottom,
}

/// A design of the Header and Footer galleries: its name and paragraphs.
pub(crate) struct Design {
    pub name: &'static str,
    /// The text of each paragraph; `\t` is a tab to the style's next stop.
    pub paras: &'static [&'static str],
}

/// The built-in designs, the same for headers and footers.
pub(crate) const DESIGNS: [Design; 2] = [
    Design {
        name: "Blank",
        paras: &["[Type here]"],
    },
    Design {
        name: "Blank (Three Columns)",
        paras: &["[Type here]\t[Type here]\t[Type here]"],
    },
];

/// The distances the Header from Top / Footer from Bottom boxes offer.
const DISTANCES: [i32; 5] = [0, 432, 720, 1080, 1440];

/// Button ids, shared by the tab, the menus and the Insert tab.
const MENU_IDS: [(HfMenu, &str); 5] = [
    (HfMenu::Header, "hf-header"),
    (HfMenu::Footer, "hf-footer"),
    (HfMenu::PageNumber, "hf-pagenum"),
    (HfMenu::HeaderFromTop, "hf-top"),
    (HfMenu::FooterFromBottom, "hf-bottom"),
];

/// The menu a button or box opens, by its id.
pub(crate) fn menu_of(id: &str) -> Option<HfMenu> {
    MENU_IDS.iter().find(|m| m.1 == id).map(|m| m.0)
}

pub(crate) fn menu_id(menu: HfMenu) -> &'static str {
    MENU_IDS.iter().find(|m| m.0 == menu).map_or("", |m| m.1)
}

fn hf(act: HfAct) -> Act {
    Act::Hf(act)
}

fn kind_name(is_header: bool) -> &'static str {
    if is_header { "Header" } else { "Footer" }
}

/// The Header or Footer menu's commands, for the ribbon's drop-down items
/// and the menu alike: the gallery, then Edit and Remove.
fn hf_menu_cmds(is_header: bool) -> Vec<rs::Cmd<Act>> {
    let k = if is_header { "header" } else { "footer" };
    let mut out: Vec<rs::Cmd<Act>> = DESIGNS
        .iter()
        .enumerate()
        .map(|(index, d)| {
            rs::cmd(
                if index == 0 {
                    if is_header { "hdr-blank" } else { "ftr-blank" }
                } else if is_header {
                    "hdr-blank3"
                } else {
                    "ftr-blank3"
                },
                k,
                d.name,
                hf(HfAct::Design { is_header, index }),
            )
        })
        .collect();
    out.push(rs::cmd(
        if is_header { "hdr-edit" } else { "ftr-edit" },
        k,
        if is_header {
            "Edit Header"
        } else {
            "Edit Footer"
        },
        hf(HfAct::Edit(is_header)),
    ));
    out.push(rs::cmd(
        if is_header {
            "hdr-remove"
        } else {
            "ftr-remove"
        },
        k,
        if is_header {
            "Remove Header"
        } else {
            "Remove Footer"
        },
        hf(HfAct::Remove(is_header)),
    ));
    out
}

/// The Header, Footer and Page Number drop-downs (the Insert tab's Header &
/// Footer group and the contextual tab's first group).
pub(crate) fn hf_dropdowns() -> Vec<Control<Act>> {
    vec![
        Control::Dropdown {
            cmd: cmdt(
                "hf-header",
                "header",
                "Header",
                hf(HfAct::Menu(HfMenu::Header)),
                "",
            )
            .tip(
                "Header",
                "Add a header, or edit or remove this section's.",
                "",
            )
            .key("H"),
            items: hf_menu_cmds(true),
        },
        Control::Dropdown {
            cmd: cmdt(
                "hf-footer",
                "footer",
                "Footer",
                hf(HfAct::Menu(HfMenu::Footer)),
                "",
            )
            .tip(
                "Footer",
                "Add a footer, or edit or remove this section's.",
                "",
            )
            .key("O"),
            items: hf_menu_cmds(false),
        },
        Control::Dropdown {
            cmd: cmdt(
                "hf-pagenum",
                "page-number",
                "Page Number",
                hf(HfAct::Menu(HfMenu::PageNumber)),
                "",
            )
            .tip("Add Page Numbers", "Number the pages of the document.", "")
            .key("NU"),
            items: crate::page_number::ribbon_items(),
        },
    ]
}

/// The contextual Header & Footer tab.
pub(crate) fn hf_tab() -> rs::Tab<Act> {
    rs::tab(
        "Header & Footer",
        "J",
        vec![
            rs::group("Header & Footer", 60, hf_dropdowns()),
            rs::group(
                "Insert",
                20,
                vec![Control::Large(
                    cmdt("hf-field", "case", "Field", Act::InsertField, "").key("Q"),
                )],
            ),
            rs::group(
                "Navigation",
                50,
                vec![
                    Control::Column(vec![
                        cmdt(
                            "hf-goto-header",
                            "header",
                            "Go to Header",
                            hf(HfAct::GoTo(true)),
                            "",
                        )
                        .key("GH"),
                        cmdt(
                            "hf-goto-footer",
                            "footer",
                            "Go to Footer",
                            hf(HfAct::GoTo(false)),
                            "",
                        )
                        .key("GF"),
                    ]),
                    Control::Column(vec![
                        cmdt("hf-prev", "undo", "Previous", hf(HfAct::Previous), "").key("PV"),
                        cmdt("hf-next", "redo", "Next", hf(HfAct::Next), "").key("NX"),
                        cmdt(
                            "hf-link",
                            "copy",
                            "Link to Previous",
                            hf(HfAct::LinkToPrevious),
                            "",
                        )
                        .tip(
                            "Link to Previous",
                            "Use the previous section's header or footer here.",
                            "",
                        )
                        .key("L"),
                    ]),
                ],
            ),
            rs::group(
                "Options",
                40,
                vec![Control::Column(vec![
                    cmdt(
                        "hf-first",
                        "",
                        "Different First Page",
                        hf(HfAct::DifferentFirst),
                        "",
                    )
                    .key("A"),
                    cmdt(
                        "hf-oddeven",
                        "",
                        "Different Odd & Even Pages",
                        hf(HfAct::DifferentOddEven),
                        "",
                    )
                    .key("V"),
                    cmdt(
                        "hf-showtext",
                        "",
                        "Show Document Text",
                        hf(HfAct::ShowText),
                        "",
                    )
                    .key("T"),
                ])],
            ),
            rs::group(
                "Position",
                30,
                vec![Control::Rows(vec![
                    vec![rs::Cell::Combo {
                        cmd: cmdt(
                            "hf-top",
                            "",
                            "Header from Top",
                            hf(HfAct::Menu(HfMenu::HeaderFromTop)),
                            "",
                        )
                        .key("PH"),
                        wide: false,
                    }],
                    vec![rs::Cell::Combo {
                        cmd: cmdt(
                            "hf-bottom",
                            "",
                            "Footer from Bottom",
                            hf(HfAct::Menu(HfMenu::FooterFromBottom)),
                            "",
                        )
                        .key("PF"),
                        wide: false,
                    }],
                ])],
            ),
            rs::group(
                "Close",
                70,
                vec![Control::Large(
                    cmdt(
                        "hf-close",
                        "table-dismiss",
                        "Close Header and Footer",
                        hf(HfAct::Close),
                        "Esc",
                    )
                    .key("C"),
                )],
            ),
        ],
    )
}

/// A menu's items, with the live checked states.
pub(crate) fn menu_items(tab: Option<&DocTab>, menu: HfMenu) -> Vec<menu::MenuItem> {
    let entry = |c: rs::Cmd<Act>| {
        menu::MenuItem::Item(menu::Entry::new(c.id, c.label, c.icon.0, c.act, true))
    };
    match menu {
        HfMenu::Header | HfMenu::Footer => {
            let is_header = menu == HfMenu::Header;
            let mut cmds = hf_menu_cmds(is_header).into_iter();
            let mut items: Vec<menu::MenuItem> = vec![menu::MenuItem::Heading("Built-in".into())];
            items.extend(cmds.by_ref().take(DESIGNS.len()).map(entry));
            items.push(menu::MenuItem::Separator);
            items.extend(cmds.map(entry));
            items
        }
        HfMenu::PageNumber => crate::page_number::menu_items(tab),
        HfMenu::HeaderFromTop | HfMenu::FooterFromBottom => {
            let is_header = menu == HfMenu::HeaderFromTop;
            let current = tab.and_then(|t| distance_of(t, is_header));
            let mut items: Vec<menu::MenuItem> = DISTANCES
                .iter()
                .map(|&twips| {
                    menu::MenuItem::Item(
                        menu::Entry::new(
                            &format!("{}-{twips}", menu_id(menu)),
                            &format!("{}\"", crate::page_setup::inches(twips)),
                            "",
                            hf(HfAct::Distance { is_header, twips }),
                            true,
                        )
                        .checked(current == Some(twips)),
                    )
                })
                .collect();
            items.push(menu::MenuItem::Separator);
            items.push(menu::MenuItem::Item(menu::Entry::new(
                &format!("{}-custom", menu_id(menu)),
                "Custom...",
                "",
                hf(HfAct::CustomDistance(is_header)),
                true,
            )));
            items
        }
    }
}

/// The section and variant the header/footer commands act on: the edited
/// slot, else the body caret's section (default variant).
pub(crate) fn target(tab: &DocTab) -> (usize, HeaderVariant) {
    match &tab.hf_edit {
        Some(h) => (h.section, h.variant),
        None => (crate::hf_section(tab), HeaderVariant::Default),
    }
}

/// The section and variant a command for the header (`is_header`) or footer
/// acts on: the edited slot when that area is the one open, else the
/// target section's default one.
pub(crate) fn target_for(tab: &DocTab, is_header: bool) -> (usize, HeaderVariant) {
    let (section, variant) = target(tab);
    match &tab.hf_edit {
        Some(h) if h.is_header == is_header => (section, variant),
        _ => (section, HeaderVariant::Default),
    }
}

fn body_sections(tab: &DocTab) -> Vec<String> {
    match &tab.surface {
        Surface::Doc(ed) => ed.sections(),
        _ => Vec::new(),
    }
}

/// The target section's header (`is_header`) or footer distance, in twips.
pub(crate) fn distance_of(tab: &DocTab, is_header: bool) -> Option<i32> {
    let (section, _) = target(tab);
    body_sections(tab)
        .get(section)
        .map(|s| crate::hf::distance(s, is_header))
}

/// What the Header from Top (`is_header`) or Footer from Bottom box shows:
/// the target section's distance in inches (`0.5"`).
pub(crate) fn distance_text(tab: &DocTab, is_header: bool) -> Option<String> {
    distance_of(tab, is_header).map(|t| format!("{}\"", crate::page_setup::inches(t)))
}

/// Whether the edited slot is linked to the previous section's ("Same as
/// Previous"): a later section with no reference of its own.
pub(crate) fn linked(tab: &DocTab) -> bool {
    let Some(h) = &tab.hf_edit else {
        return false;
    };
    h.section > 0
        && body_sections(tab)
            .get(h.section)
            .is_some_and(|s| hf_reference(s, h.is_header, h.variant.as_ooxml()).is_none())
}

/// Word's tab label for an area (PAG-067): Header, First Page Header, Odd or
/// Even Page Header (Footer alike), with " -Section n-" when the document
/// has more than one section.
pub(crate) fn area_label(
    is_header: bool,
    variant: HeaderVariant,
    even_odd: bool,
    section: usize,
    sections: usize,
) -> String {
    let kind = kind_name(is_header);
    let base = match variant {
        HeaderVariant::First => format!("First Page {kind}"),
        HeaderVariant::Even => format!("Even Page {kind}"),
        HeaderVariant::Default if even_odd => format!("Odd Page {kind}"),
        HeaderVariant::Default => kind.to_string(),
    };
    if sections > 1 {
        format!("{base} -Section {}-", section + 1)
    } else {
        base
    }
}

/// The open area's label, if a header or footer is being edited.
pub(crate) fn edit_label(tab: &DocTab) -> Option<String> {
    let h = tab.hf_edit.as_ref()?;
    let even_odd = tab.pkg.as_ref().is_some_and(|p| p.has_even_odd());
    Some(area_label(
        h.is_header,
        h.variant,
        even_odd,
        h.section,
        body_sections(tab).len(),
    ))
}

/// Whether a command's check mark is on.
pub(crate) fn hf_checked(tab: &DocTab, act: HfAct) -> bool {
    let (section, _) = target(tab);
    let sect = body_sections(tab).get(section).cloned().unwrap_or_default();
    match act {
        HfAct::LinkToPrevious => linked(tab),
        HfAct::DifferentFirst => docxcore::sect::has_flag(&sect, "w:titlePg"),
        HfAct::DifferentOddEven => tab.pkg.as_ref().is_some_and(|p| p.has_even_odd()),
        HfAct::ShowText => tab.hf_edit.as_ref().is_none_or(|h| h.show_text),
        HfAct::GoTo(is_header) => tab
            .hf_edit
            .as_ref()
            .is_some_and(|h| h.is_header == is_header),
        _ => false,
    }
}

/// The start of an existing header/footer part up to and including its root
/// start tag (the XML declaration, `<w:hdr …>` with every namespace and
/// `mc:Ignorable` it declares), and the end tag that closes it. `None` when
/// the part holds no such root.
fn root_of(xml: &str, is_header: bool) -> Option<(String, String)> {
    let tag = if is_header { "w:hdr" } else { "w:ftr" };
    let open = format!("<{tag}");
    let mut from = 0;
    let start = loop {
        let at = from + xml[from..].find(&open)?;
        let after = &xml[at + open.len()..];
        if after.starts_with([' ', '>', '/', '\t', '\r', '\n']) {
            break at;
        }
        from = at + open.len();
    };
    let gt = start + xml[start..].find('>')?;
    let close = format!("</{tag}>");
    if xml[..gt].ends_with('/') {
        // `<w:hdr …/>`: an empty part; open it up.
        return Some((format!("{}>", &xml[..gt - 1]), close));
    }
    Some((xml[..=gt].to_string(), close))
}

/// Replace an existing header/footer part's content with `inner` (block XML),
/// keeping the part's own root start tag, so the namespaces its drawings,
/// VML and markup-compatibility content use stay declared. A part without a
/// readable root gets a fresh one ([`part_xml`]).
pub(crate) fn rewrite_part(pkg: &mut Package, part: &str, is_header: bool, inner: &str) {
    let existing = pkg
        .part(part)
        .map(|b| String::from_utf8_lossy(b).into_owned());
    let xml = match existing.as_deref().and_then(|x| root_of(x, is_header)) {
        Some((head, tail)) => format!("{head}{inner}{tail}"),
        None => part_xml(is_header, inner),
    };
    pkg.set_part(part, xml.into_bytes());
}

/// The header/footer XML a new part holds, around its block content.
pub(crate) fn part_xml(is_header: bool, inner: &str) -> String {
    let tag = if is_header { "w:hdr" } else { "w:ftr" };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <{tag} xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:m=\"{M_NS}\">{inner}</{tag}>"
    )
}

/// A design's block XML, in the Header or Footer style.
pub(crate) fn design_xml(is_header: bool, design: &Design) -> String {
    let style = crate::hf::style_id(is_header);
    design
        .paras
        .iter()
        .map(|p| {
            let runs: Vec<String> = p
                .split('\t')
                .map(|t| {
                    format!(
                        "<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r>",
                        xml_escape(t)
                    )
                })
                .collect();
            format!(
                "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr>{}</w:p>",
                runs.join("<w:r><w:tab/></w:r>")
            )
        })
        .collect()
}

/// Replace a header/footer part's content with `inner` (block XML), or, when
/// `section`/`variant` resolves to none, create the part (see
/// [`crate::hf::create_for`]). The part's name.
pub(crate) fn set_content(
    tab: &mut DocTab,
    section: usize,
    is_header: bool,
    variant: HeaderVariant,
    inner: &str,
) -> Result<String, String> {
    let resolved = resolved_part(tab, section, is_header, variant);
    match resolved {
        Some(name) => {
            let pkg = tab.pkg.as_mut().ok_or("Headers/footers need a .docx")?;
            pkg.ensure_styles(&[crate::hf::style_id(is_header)]);
            rewrite_part(pkg, &name, is_header, inner);
            tab.dirty = true;
            Ok(name)
        }
        None => crate::hf::create_for(tab, section, is_header, variant, inner)
            .ok_or_else(|| "Could not create the header/footer part".into()),
    }
}

/// The part `section`/`variant` resolves to, if any.
pub(crate) fn resolved_part(
    tab: &DocTab,
    section: usize,
    is_header: bool,
    variant: HeaderVariant,
) -> Option<String> {
    let (Surface::Doc(ed), Some(pkg)) = (&tab.surface, tab.pkg.as_ref()) else {
        return None;
    };
    crate::hf::resolve(ed, pkg)
        .get(section)?
        .get(is_header, variant)
        .map(|a| a.part_name.clone())
}

/// (Re)open the header/footer editor on `section`/`variant` (after its part
/// or link changed, or to move to another area), keeping Show Document Text.
pub(crate) fn reopen(
    tab: &mut DocTab,
    section: usize,
    is_header: bool,
    variant: HeaderVariant,
) -> bool {
    let show_text = tab.hf_edit.as_ref().is_none_or(|h| h.show_text);
    tab.hf_edit = None;
    let ok = crate::hf::open(tab, section, is_header, variant);
    if let Some(h) = tab.hf_edit.as_mut() {
        h.show_text = show_text;
    }
    ok
}

/// The slot each print-layout page shows, in page order.
pub(crate) fn tab_page_slots(tab: &DocTab) -> Vec<PageSlot> {
    let Surface::Doc(ed) = &tab.surface else {
        return Vec::new();
    };
    let even_odd = tab.pkg.as_ref().is_some_and(|p| p.has_even_odd());
    let firsts: Vec<usize> = crate::page_ranges(tab)
        .iter()
        .map(|cols| cols.first().map_or(0, |c| c.0))
        .collect();
    crate::hf::page_slots(ed, &firsts, even_odd)
}

/// The navigation order Previous and Next walk: each (section, variant) slot
/// that some page shows, in page order.
pub(crate) fn nav_slots(tab: &DocTab) -> Vec<PageSlot> {
    let mut out: Vec<PageSlot> = Vec::new();
    for slot in tab_page_slots(tab) {
        if !out.contains(&slot) {
            out.push(slot);
        }
    }
    out
}

/// The slot print-layout page `page` (0-based) shows, if there is one.
pub(crate) fn page_slot(tab: &DocTab, page: usize) -> Option<PageSlot> {
    tab_page_slots(tab).get(page).copied()
}

fn variant_rank(v: HeaderVariant) -> u8 {
    match v {
        HeaderVariant::First => 0,
        HeaderVariant::Default => 1,
        HeaderVariant::Even => 2,
    }
}

/// The slot Next (`forward`) or Previous moves to from the edited one, if any.
fn step(tab: &DocTab, forward: bool) -> Option<PageSlot> {
    let h = tab.hf_edit.as_ref()?;
    let slots = nav_slots(tab);
    let here = PageSlot {
        section: h.section,
        variant: h.variant,
    };
    let key = |s: &PageSlot| (s.section, variant_rank(s.variant));
    match slots.iter().position(|s| *s == here) {
        Some(i) if forward => slots.get(i + 1).copied(),
        Some(i) => i.checked_sub(1).map(|j| slots[j]),
        // A slot no page shows: the neighbour in section/variant order.
        None if forward => slots.iter().find(|s| key(s) > key(&here)).copied(),
        None => slots.iter().rev().find(|s| key(s) < key(&here)).copied(),
    }
}

/// Run a Header & Footer command that works on the tab alone (every one but
/// the menus, dialogs and Close). The status says what happened; an error
/// says why nothing did.
pub(crate) fn hf_apply(tab: &mut DocTab, act: HfAct) -> Result<(), String> {
    if tab.pkg.is_none() || !matches!(tab.surface, Surface::Doc(_)) {
        return Err("Headers/footers need a .docx (not a Markdown document)".into());
    }
    flush_hf_tab(tab);
    let (section, variant) = target(tab);
    let is_header_now = tab.hf_edit.as_ref().is_none_or(|h| h.is_header);
    match act {
        HfAct::Design { is_header, index } => {
            let design = DESIGNS.get(index).ok_or("no such design")?;
            // The design goes in the edited slot when it is of this kind,
            // else in the section's default one.
            let (_, variant) = target_for(tab, is_header);
            set_content(
                tab,
                section,
                is_header,
                variant,
                &design_xml(is_header, design),
            )?;
            reopen(tab, section, is_header, variant);
            tab.status = format!("{}: {}", kind_name(is_header), design.name).into();
        }
        HfAct::Edit(is_header) => {
            let (_, variant) = target_for(tab, is_header);
            if !reopen(tab, section, is_header, variant) {
                return Err(tab.status.to_string());
            }
        }
        HfAct::Remove(is_header) => {
            let (_, variant) = target_for(tab, is_header);
            let name = resolved_part(tab, section, is_header, variant).ok_or_else(|| {
                format!(
                    "This section has no {}",
                    kind_name(is_header).to_lowercase()
                )
            })?;
            let pkg = tab.pkg.as_mut().ok_or("no package")?;
            rewrite_part(pkg, &name, is_header, &crate::hf::empty_content(is_header));
            tab.dirty = true;
            if tab.hf_edit.as_ref().is_some_and(|h| h.part_name == name) {
                reopen(tab, section, is_header, variant);
            }
            tab.status = format!("Removed the {}", kind_name(is_header).to_lowercase()).into();
        }
        HfAct::GoTo(is_header) => {
            if tab.hf_edit.is_none() {
                return Err("Go to Header works while a header or footer is open".into());
            }
            reopen(tab, section, is_header, variant);
        }
        HfAct::Previous | HfAct::Next => {
            let is_header = is_header_now;
            // At either end, and with no page for the next slot, nothing happens.
            if let Some(to) = step(tab, act == HfAct::Next) {
                reopen(tab, to.section, is_header, to.variant);
            }
        }
        HfAct::LinkToPrevious => {
            let Some(h) = tab.hf_edit.as_ref() else {
                return Err("Link to Previous works while a header or footer is open".into());
            };
            let is_header = h.is_header;
            if section == 0 {
                return Err("The first section has no previous section to link to".into());
            }
            let kind = variant.as_ooxml();
            if linked(tab) {
                // Unlink: this section gets its own copy of what it showed.
                let inherited = resolved_part(tab, section, is_header, variant);
                let pkg = tab.pkg.as_mut().ok_or("no package")?;
                let (rid, _) = match inherited {
                    Some(src) => pkg.copy_hf_part(&src),
                    None => pkg.create_hf_part(is_header, &crate::hf::empty_content(is_header)),
                }
                .ok_or("Could not copy the header/footer part")?;
                let Surface::Doc(ed) = &mut tab.surface else {
                    return Err("no document".into());
                };
                ed.edit_sections(&[section], |raw| {
                    set_hf_reference(raw, is_header, kind, Some(&rid))
                });
                tab.status = "Link to Previous: off".into();
            } else {
                // Relink: drop this section's own reference; the part stays
                // (an undo of the relink needs it back).
                let Surface::Doc(ed) = &mut tab.surface else {
                    return Err("no document".into());
                };
                ed.edit_sections(&[section], |raw| {
                    set_hf_reference(raw, is_header, kind, None)
                });
                tab.status = "Link to Previous: on".into();
            }
            tab.dirty = true;
            reopen(tab, section, is_header, variant);
        }
        HfAct::DifferentFirst => {
            let on = toggle_title_pg_tab(tab);
            if !on && variant == HeaderVariant::First {
                reopen(tab, section, is_header_now, HeaderVariant::Default);
            }
        }
        HfAct::DifferentOddEven => {
            let pkg = tab.pkg.as_mut().ok_or("no package")?;
            let on = !pkg.has_even_odd();
            pkg.set_even_odd(on);
            tab.dirty = true;
            if !on && variant == HeaderVariant::Even {
                reopen(tab, section, is_header_now, HeaderVariant::Default);
            }
        }
        HfAct::ShowText => {
            let h = tab
                .hf_edit
                .as_mut()
                .ok_or("Show Document Text works while a header or footer is open")?;
            h.show_text = !h.show_text;
        }
        HfAct::Distance { is_header, twips } => {
            let Surface::Doc(ed) = &mut tab.surface else {
                return Err("no document".into());
            };
            let changed = ed.edit_section_setups(&[section], |s| {
                if is_header {
                    s.margins.header = twips;
                } else {
                    s.margins.footer = twips;
                }
            });
            tab.dirty |= changed;
            let name = if is_header {
                "Header from Top"
            } else {
                "Footer from Bottom"
            };
            tab.status = format!("{name}: {}\"", crate::page_setup::inches(twips)).into();
        }
        HfAct::Menu(_) | HfAct::CustomDistance(_) | HfAct::Close | HfAct::PageNumber(_) => {}
    }
    Ok(())
}

/// The Header from Top / Footer from Bottom Custom... dialog: one distance,
/// in inches, for the target section.
pub(crate) fn distance_dialog(tab: &DocTab, is_header: bool) -> Result<Dialog, String> {
    let twips = distance_of(tab, is_header).ok_or("Headers/footers need a .docx")?;
    let (section, _) = target(tab);
    let title = if is_header {
        "Header from Top"
    } else {
        "Footer from Bottom"
    };
    let mut d = Dialog::message(
        "hf-distance",
        title,
        String::new(),
        &[],
        DialogOwner::HfDistance { is_header, section },
    );
    d.text = None;
    d.controls = vec![Field::new(
        "distance",
        &format!("{title} (inches):"),
        ControlKind::Number,
        Value::Text(crate::page_setup::inches(twips)),
    )];
    d.buttons = vec![
        Button {
            default: true,
            ..Button::new("OK", ButtonRole::Accept)
        },
        Button::new("Cancel", ButtonRole::Cancel),
    ];
    d.mark_opened();
    Ok(d)
}

/// OK on the distance dialog: write the distance into the section.
pub(crate) fn apply_distance(
    ed: &mut Editor,
    d: &Dialog,
    is_header: bool,
    section: usize,
) -> Result<bool, String> {
    let text = d
        .controls
        .iter()
        .find(|c| c.name == "distance")
        .map(|c| c.text())
        .unwrap_or_default();
    let v = text
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && (0.0..=22.0).contains(v))
        .ok_or("Enter a distance from 0\" to 22\"")?;
    let twips = (v * TWIPS_PER_INCH as f64).round() as i32;
    Ok(ed.edit_section_setups(&[section], |s| {
        if is_header {
            s.margins.header = twips;
        } else {
            s.margins.footer = twips;
        }
    }))
}

/// `hf-state`: whether a header or footer is being edited and, if so, which
/// one, its labels and the tab's Options and Position values.
pub(crate) fn hf_state(tab: Option<&DocTab>) -> Json {
    let Some(tab) = tab else {
        return Json::obj(vec![("editing", Json::Bool(false))]);
    };
    let inches = |t: Option<i32>| t.map_or(Json::Null, |t| Json::Num(t as f64 / 1440.0));
    let mut fields = vec![("editing", Json::Bool(tab.hf_edit.is_some()))];
    if let Some(h) = &tab.hf_edit {
        fields.extend([
            (
                "kind",
                Json::Str(if h.is_header { "header" } else { "footer" }.into()),
            ),
            ("section", Json::Num((h.section + 1) as f64)),
            ("variant", Json::Str(h.variant.as_ooxml().into())),
            ("label", Json::Str(edit_label(tab).unwrap_or_default())),
            ("same_as_previous", Json::Bool(linked(tab))),
            ("part", Json::Str(h.part_name.clone())),
            (
                "text",
                Json::Str(
                    h.editor
                        .doc
                        .body
                        .iter()
                        // Content-control boundaries (a placed page number's) hold no text.
                        .filter(|b| !matches!(b, Block::Raw(_)))
                        .map(Block::plain_text)
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
            ),
            ("show_document_text", Json::Bool(h.show_text)),
            (
                "page_numbers",
                Json::Num(
                    h.editor
                        .doc
                        .body
                        .iter()
                        .filter(|b| docxcore::hf::is_page_number_open(b))
                        .count() as f64,
                ),
            ),
        ]);
    }
    fields.extend([
        (
            "different_first_page",
            Json::Bool(hf_checked(tab, HfAct::DifferentFirst)),
        ),
        (
            "different_odd_even",
            Json::Bool(hf_checked(tab, HfAct::DifferentOddEven)),
        ),
        ("header_from_top", inches(distance_of(tab, true))),
        ("footer_from_bottom", inches(distance_of(tab, false))),
    ]);
    Json::obj(fields)
}

impl Docxy {
    /// Dispatch a Header & Footer command.
    pub(crate) fn hf_act(&mut self, act: HfAct, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            HfAct::Menu(menu) => {
                let id = menu_id(menu);
                let at = split_menu_anchor(&self.probes.borrow(), id)
                    .unwrap_or_else(|| point(px(160.), px(140.)));
                if let Err(e) = self.open_split_menu(id, at, cx) {
                    self.set_status(e);
                }
                return;
            }
            HfAct::Close => return self.exit_hf(window, cx),
            HfAct::CustomDistance(is_header) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    flush_hf_tab(tab);
                    match distance_dialog(tab, is_header) {
                        Ok(d) => tab.dialogs.push(d),
                        Err(e) => tab.status = e.into(),
                    }
                }
            }
            HfAct::PageNumber(pn) => return self.page_number_act(pn, window, cx),
            _ => {
                let was_open = self.hf_active();
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if let Err(e) = hf_apply(tab, act) {
                        tab.status = e.into();
                    }
                }
                // A command that opened the header area shows it, with its tab.
                if !was_open && self.hf_active() {
                    self.page_view = true;
                    self.ribbon_tab = RibbonTab::HeaderFooter;
                }
                self.scroll_to_hf();
            }
        }
        self.refocus(window, cx);
    }

    /// Bring the page on which the open header or footer is edited into view
    /// (after Previous, Next, Go to and the like move to another page).
    pub(crate) fn scroll_to_hf(&self) {
        if !self.page_view {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Some(h) = &tab.hf_edit else {
            return;
        };
        let slots = tab_page_slots(tab);
        self.doc_scroll
            .scroll_to_item(crate::hf::edit_page(&slots, h.section, h.variant));
    }
}

#[cfg(test)]
mod tests;
