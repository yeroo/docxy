//! Insert › Page Number (#650): Word's menu of Top of Page, Bottom of Page,
//! Page Margins and Current Position designs, Format Page Numbers... and
//! Remove Page Numbers.
//!
//! A number placed at the top or bottom of the page is Word's own shape: a
//! `docPartObj` content control (`Page Numbers (Top of Page)` / `(Bottom of
//! Page)`) around one paragraph in the caret section's header or footer (see
//! [`docxcore::hf`]). Choosing another design replaces it, so a section never
//! holds two, and Remove Page Numbers removes exactly those controls, ours
//! and Word's, leaving `PAGE` fields typed into the body or a header alone.
use super::*;
use crate::dialog::catalog;
use crate::dialog::{Control, ControlKind, Dialog, DialogOwner, Value};
use crate::hf_tab::{HfAct, reopen, resolved_part, rewrite_part, set_content, target, target_for};
use crate::page_setup::{choice, chosen, ok_cancel, text_of};
use docxcore::hf::{PAGE_NUMBER_DESIGNS, page_number_sdt_xml, remove_page_numbers};
use docxcore::sect::PageNumberFormat;

/// A Page Number menu command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PnAct {
    /// A Top of Page (`top`) or Bottom of Page design, by index.
    Place {
        top: bool,
        design: usize,
    },
    /// A Current Position design: the number at the caret, unmarked.
    Current {
        design: usize,
    },
    /// Open the Page Number Format dialog.
    Format,
    Remove,
}

fn pn(act: PnAct) -> Act {
    Act::Hf(HfAct::PageNumber(act))
}

/// The Current Position gallery: a bare number, and "Page X of Y".
const CURRENT: [(&str, bool); 2] = [("Plain Number", false), ("Page X of Y", true)];

fn place_id(top: bool, design: usize) -> &'static str {
    const TOP: [&str; 4] = ["pn-top-1", "pn-top-2", "pn-top-3", "pn-top-xy"];
    const BOTTOM: [&str; 4] = ["pn-bottom-1", "pn-bottom-2", "pn-bottom-3", "pn-bottom-xy"];
    if top { TOP[design] } else { BOTTOM[design] }
}

/// The drop-down's commands for `ribbon-read` and `ribbon-click`: each
/// gallery item named with its gallery ("Bottom of Page: Plain Number 2"),
/// then Format Page Numbers... and Remove Page Numbers.
pub(crate) fn ribbon_items() -> Vec<rs::Cmd<Act>> {
    const TOP: [&str; 4] = [
        "Top of Page: Plain Number 1",
        "Top of Page: Plain Number 2",
        "Top of Page: Plain Number 3",
        "Top of Page: Page X of Y",
    ];
    const BOTTOM: [&str; 4] = [
        "Bottom of Page: Plain Number 1",
        "Bottom of Page: Plain Number 2",
        "Bottom of Page: Plain Number 3",
        "Bottom of Page: Page X of Y",
    ];
    const CURRENT_IDS: [(&str, &str); 2] = [
        ("pn-current-1", "Current Position: Plain Number"),
        ("pn-current-xy", "Current Position: Page X of Y"),
    ];
    let mut out = Vec::new();
    for (top, labels) in [(true, TOP), (false, BOTTOM)] {
        for (design, label) in labels.into_iter().enumerate() {
            out.push(rs::cmd(
                place_id(top, design),
                "page-number",
                label,
                pn(PnAct::Place { top, design }),
            ));
        }
    }
    for (design, (id, label)) in CURRENT_IDS.into_iter().enumerate() {
        out.push(rs::cmd(
            id,
            "page-number",
            label,
            pn(PnAct::Current { design }),
        ));
    }
    out.push(rs::cmd(
        "pn-format",
        "page-number",
        "Format Page Numbers...",
        pn(PnAct::Format),
    ));
    out.push(rs::cmd(
        "pn-remove",
        "page-number",
        "Remove Page Numbers",
        pn(PnAct::Remove),
    ));
    out
}

/// The Page Number menu (Screen PAG14): four galleries as submenus, then
/// Format Page Numbers... and Remove Page Numbers.
pub(crate) fn menu_items(_tab: Option<&DocTab>) -> Vec<menu::MenuItem> {
    use menu::{Entry, MenuItem};
    let gallery = |id: &str, label: &str, items: Vec<MenuItem>| {
        let mut e = Entry::unavailable(id, label);
        e.enabled = true;
        e.submenu = items;
        MenuItem::Item(e)
    };
    let placed = |top: bool| {
        PAGE_NUMBER_DESIGNS
            .iter()
            .enumerate()
            .map(|(design, d)| {
                MenuItem::Item(Entry::new(
                    place_id(top, design),
                    d.name,
                    "",
                    pn(PnAct::Place { top, design }),
                    true,
                ))
            })
            .collect()
    };
    let current = CURRENT
        .iter()
        .enumerate()
        .map(|(design, (name, _))| {
            MenuItem::Item(Entry::new(
                if design == 0 {
                    "pn-current-1"
                } else {
                    "pn-current-xy"
                },
                name,
                "",
                pn(PnAct::Current { design }),
                true,
            ))
        })
        .collect();
    vec![
        gallery("pn-top", "Top of Page", placed(true)),
        gallery("pn-bottom", "Bottom of Page", placed(false)),
        // Word's side-margin designs need a floating frame the suite cannot
        // place yet: listed for order parity, unavailable.
        MenuItem::Item(Entry::unavailable("pn-margins", "Page Margins")),
        gallery("pn-current", "Current Position", current),
        MenuItem::Separator,
        MenuItem::Item(Entry::new(
            "pn-format",
            "Format Page Numbers...",
            "",
            pn(PnAct::Format),
            true,
        )),
        MenuItem::Item(Entry::new(
            "pn-remove",
            "Remove Page Numbers",
            "",
            pn(PnAct::Remove),
            true,
        )),
    ]
}

/// A content-control id for a new placed number: Word writes a random
/// signed 32-bit value; any distinct one will do.
fn sdt_id() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.subsec_nanos());
    (nanos % 0x7fff_ffff).max(1)
}

/// Remove the placed page numbers from one part. Whether any went.
fn strip_part(tab: &mut DocTab, part: &str, is_header: bool) -> bool {
    let Some(pkg) = tab.pkg.as_mut() else {
        return false;
    };
    let mut blocks = parse_hf_part(pkg, part);
    if !remove_page_numbers(&mut blocks) {
        return false;
    }
    let inner = docxcore::serialize::blocks_to_xml(&blocks);
    rewrite_part(pkg, part, is_header, &inner);
    tab.set_dirty();
    true
}

/// Whether a part's blocks hold nothing a person wrote: only empty paragraphs.
fn blank(blocks: &[Block]) -> bool {
    blocks.iter().all(
        |b| matches!(b, Block::Paragraph(p) if p.plain_text().is_empty() && p.content.is_empty()),
    )
}

/// Top of Page / Bottom of Page: put the design in the target section's
/// header (`top`) or footer, replacing a number placed there before and
/// removing one placed in the other area of the same section (Word moves
/// it), then open that area for editing. The variant is the edited one when
/// that area is open, else the default one.
pub(crate) fn place(tab: &mut DocTab, top: bool, design: usize) -> Result<(), String> {
    if tab.pkg.is_none() || !matches!(tab.surface, Surface::Doc(_)) {
        return Err("Page numbers need a .docx (not a Markdown document)".into());
    }
    let d = PAGE_NUMBER_DESIGNS.get(design).ok_or("no such design")?;
    flush_hf_tab(tab);
    let is_header = top;
    let (section, variant) = target_for(tab, is_header);
    if let Some(other) = resolved_part(tab, section, !is_header, variant) {
        strip_part(tab, &other, !is_header);
    }
    let rest = match (
        resolved_part(tab, section, is_header, variant),
        tab.pkg.as_ref(),
    ) {
        (Some(part), Some(pkg)) => {
            let mut blocks = parse_hf_part(pkg, &part);
            remove_page_numbers(&mut blocks);
            if blank(&blocks) {
                String::new()
            } else {
                docxcore::serialize::blocks_to_xml(&blocks)
            }
        }
        _ => String::new(),
    };
    let number = page_number_sdt_xml(d, top, sdt_id());
    let inner = if top {
        format!("{number}{rest}")
    } else {
        format!("{rest}{number}")
    };
    if let Some(pkg) = tab.pkg.as_mut() {
        pkg.ensure_styles(&["Header", "Footer"]);
    }
    set_content(tab, section, is_header, variant, &inner)?;
    reopen(tab, section, is_header, variant);
    let gallery = if top { "Top of Page" } else { "Bottom of Page" };
    tab.status = format!("Page number: {gallery}, {}", d.name).into();
    Ok(())
}

/// Remove Page Numbers: every placed number in every header and footer the
/// document's sections show. Whether any went.
pub(crate) fn remove_all(tab: &mut DocTab) -> Result<bool, String> {
    if tab.pkg.is_none() || !matches!(tab.surface, Surface::Doc(_)) {
        return Err("Page numbers need a .docx (not a Markdown document)".into());
    }
    flush_hf_tab(tab);
    let (Surface::Doc(ed), Some(pkg)) = (&tab.surface, tab.pkg.as_ref()) else {
        return Ok(false);
    };
    let mut parts: Vec<(String, bool)> = Vec::new();
    for sp in crate::hf::resolve(ed, pkg) {
        for (is_header, slots) in [(true, &sp.headers), (false, &sp.footers)] {
            for a in slots.iter().flatten() {
                if !parts.iter().any(|(n, _)| *n == a.part_name) {
                    parts.push((a.part_name.clone(), is_header));
                }
            }
        }
    }
    let mut any = false;
    for (part, is_header) in &parts {
        any |= strip_part(tab, part, *is_header);
    }
    if let Some(h) = tab.hf_edit.as_ref() {
        let (section, is_header, variant) = (h.section, h.is_header, h.variant);
        if parts.iter().any(|(n, _)| *n == h.part_name) {
            reopen(tab, section, is_header, variant);
        }
    }
    tab.status = if any {
        "Removed page numbers"
    } else {
        "No page numbers to remove"
    }
    .into();
    Ok(any)
}

/// Number format choices: the label and `w:fmt` (`None` is decimal).
const FORMATS: [(&str, Option<&str>); 6] = [
    ("1, 2, 3, ...", None),
    ("- 1 -, - 2 -, - 3 -, ...", Some("numberInDash")),
    ("a, b, c, ...", Some("lowerLetter")),
    ("A, B, C, ...", Some("upperLetter")),
    ("i, ii, iii, ...", Some("lowerRoman")),
    ("I, II, III, ...", Some("upperRoman")),
];
const CHAPTER_STYLES: [&str; 9] = [
    "Heading 1",
    "Heading 2",
    "Heading 3",
    "Heading 4",
    "Heading 5",
    "Heading 6",
    "Heading 7",
    "Heading 8",
    "Heading 9",
];
const SEPARATORS: [(&str, &str); 5] = [
    ("- (hyphen)", "hyphen"),
    (". (period)", "period"),
    (": (colon)", "colon"),
    ("\u{2014} (em dash)", "emDash"),
    ("\u{2013} (en dash)", "enDash"),
];
const NUMBERING: [&str; 2] = ["Continue from previous section", "Start at:"];

/// A drop-down over `items` on the section's value: its item, or, for a
/// value the list does not name (a `w:fmt` of `decimalZero`, say), one more
/// item showing that value, so OK can write it back unchanged.
fn pick(
    name: &'static str,
    label: &str,
    items: &[&str],
    at: Option<usize>,
    raw: Option<String>,
) -> Control {
    let mut c = choice(name, label, ControlKind::Dropdown, items, at, None);
    if at.is_none() {
        match raw {
            Some(raw) => {
                c.items.push(raw);
                c.value = Value::Choice(Some(items.len()));
            }
            None => c.value = Value::Choice(Some(0)),
        }
    }
    c
}

fn set_enabled(d: &mut Dialog, name: &str, on: bool) {
    if let Some(c) = d.controls.iter_mut().find(|c| c.name == name) {
        c.enabled = on;
    }
}

/// Enable the controls that depend on another: the chapter ones while
/// Include chapter number is checked, Start at's box while it is selected.
fn sync_enabled(d: &mut Dialog) {
    let chapter = d.value("chapter") == Some(&Value::Bool(true));
    for name in ["chap_style", "chap_sep", "examples"] {
        set_enabled(d, name, chapter);
    }
    let start = chosen(d, "numbering") == Some(1);
    set_enabled(d, "start", start);
}

fn after_set(d: &mut Dialog, i: usize, _before: &Value) {
    if d.controls[i].name == "numbering" && chosen(d, "numbering") == Some(1) {
        if let Some(c) = d.controls.iter_mut().find(|c| c.name == "start") {
            if c.text().trim().is_empty() {
                c.value = Value::Text("1".into());
            }
        }
    }
    sync_enabled(d);
}

/// The Page Number Format dialog (Screen PAG15) for the target section, on
/// its current `w:pgNumType`.
pub(crate) fn format_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Doc(ed) = &tab.surface else {
        return Err("Page numbers need a document".into());
    };
    if tab.pkg.is_none() {
        return Err("Page numbers need a .docx (not a Markdown document)".into());
    }
    let (section, _) = target(tab);
    let sections = ed.sections();
    let f = PageNumberFormat::parse(sections.get(section).map_or("", String::as_str));
    let fmt = FORMATS.iter().position(|(_, v)| *v == f.fmt.as_deref());
    let style = f
        .chap_style
        .and_then(|n| usize::try_from(n - 1).ok())
        .filter(|&n| n < CHAPTER_STYLES.len());
    let sep = f
        .chap_sep
        .as_deref()
        .and_then(|v| SEPARATORS.iter().position(|s| s.1 == v));
    let mut d = Dialog::message(
        catalog::PAGE_NUMBER_FORMAT,
        "Page Number Format",
        String::new(),
        &[],
        DialogOwner::PageNumberFormat { section },
    );
    d.text = None;
    let formats: Vec<&str> = FORMATS.iter().map(|f| f.0).collect();
    let mut examples = Control::new(
        "examples",
        "Examples:",
        ControlKind::Label,
        Value::Text("1-1, 1-A".into()),
    );
    examples.enabled = false;
    d.controls = vec![
        pick("format", "Number &format:", &formats, fmt, f.fmt.clone()),
        Control::new(
            "chapter",
            "Include chapter &number",
            ControlKind::Checkbox,
            Value::Bool(f.chap_style.is_some()),
        ),
        pick(
            "chap_style",
            "Chapter starts with st&yle:",
            &CHAPTER_STYLES,
            style,
            f.chap_style.map(|n| n.to_string()),
        ),
        pick(
            "chap_sep",
            "&Use separator:",
            &SEPARATORS.map(|s| s.0),
            sep,
            f.chap_sep.clone(),
        ),
        examples,
        choice(
            "numbering",
            "Page numbering",
            ControlKind::Radio,
            &NUMBERING,
            Some(usize::from(f.start.is_some())),
            None,
        ),
        Control::new(
            "start",
            "St&art at:",
            ControlKind::Number,
            Value::Text(f.start.map(|s| s.to_string()).unwrap_or_default()),
        ),
    ];
    sync_enabled(&mut d);
    d.buttons = ok_cancel();
    d.react = Some(crate::dialog::Reaction(after_set));
    d.mark_opened();
    Ok(d)
}

/// The format the dialog shows, over the section's current one (`orig`):
/// a part the person left alone keeps its value, so a `w:fmt`, `w:chapStyle`
/// or `w:chapSep` the lists do not name survives OK unchanged.
pub(crate) fn format_of(d: &Dialog, orig: &PageNumberFormat) -> Result<PageNumberFormat, String> {
    let fmt = if d.changed("format") {
        match chosen(d, "format").and_then(|i| FORMATS.get(i)) {
            Some(f) => f.1.map(str::to_string),
            None => orig.fmt.clone(),
        }
    } else {
        orig.fmt.clone()
    };
    let chapter_changed = ["chapter", "chap_style", "chap_sep"]
        .into_iter()
        .any(|n| d.changed(n));
    let (chap_style, chap_sep) = if !chapter_changed {
        (orig.chap_style, orig.chap_sep.clone())
    } else if d.value("chapter") == Some(&Value::Bool(true)) {
        let style = match chosen(d, "chap_style") {
            Some(i) if i < CHAPTER_STYLES.len() => Some(i as i32 + 1),
            _ => orig.chap_style.or(Some(1)),
        };
        let sep = match chosen(d, "chap_sep").and_then(|i| SEPARATORS.get(i)) {
            Some(s) => Some(s.1.to_string()),
            None => orig.chap_sep.clone().or(Some("hyphen".into())),
        };
        (style, sep)
    } else {
        (None, None)
    };
    let start = if !d.changed("numbering") && !d.changed("start") {
        orig.start
    } else if chosen(d, "numbering") == Some(1) {
        Some(
            text_of(d, "start")
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or("Start at must be a whole number, 0 or more")?,
        )
    } else {
        None
    };
    Ok(PageNumberFormat {
        fmt,
        start,
        chap_style,
        chap_sep,
    })
}

/// OK on the Page Number Format dialog: write `w:pgNumType` into the section
/// as one undo step. Whether it changed.
pub(crate) fn apply_format(ed: &mut Editor, d: &Dialog, section: usize) -> Result<bool, String> {
    let orig = PageNumberFormat::parse(ed.sections().get(section).map_or("", String::as_str));
    let f = format_of(d, &orig)?;
    Ok(ed.edit_sections(&[section], |raw| f.apply(raw)))
}

impl Docxy {
    /// Dispatch a Page Number menu command.
    pub(crate) fn page_number_act(
        &mut self,
        act: PnAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match act {
            PnAct::Place { top, design } => {
                let was_open = self.hf_active();
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if let Err(e) = place(tab, top, design) {
                        tab.status = e.into();
                    }
                }
                if !was_open && self.hf_active() {
                    self.page_view = true;
                    self.ribbon_tab = RibbonTab::HeaderFooter;
                }
                self.scroll_to_hf();
            }
            PnAct::Current { design } => {
                if CURRENT[design].1 {
                    self.insert_page_x_of_y(window, cx);
                } else {
                    self.insert_field("PAGE", "1", window, cx);
                }
                return;
            }
            PnAct::Format => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    flush_hf_tab(tab);
                    match format_dialog(tab) {
                        Ok(d) => tab.dialogs.push(d),
                        Err(e) => tab.status = e.into(),
                    }
                }
            }
            PnAct::Remove => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if let Err(e) = remove_all(tab) {
                        tab.status = e.into();
                    }
                }
            }
        }
        self.refocus(window, cx);
    }

    /// Current Position › Page X of Y: "Page {PAGE} of {NUMPAGES}" at the caret.
    fn insert_page_x_of_y(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = |instr: &str| {
            let raw = format!(
                "<w:fldSimple w:instr=\" {instr} \"><w:r><w:t xml:space=\"preserve\">1</w:t></w:r></w:fldSimple>"
            );
            Inline::Field {
                raw,
                text: "1".into(),
            }
        };
        let text = |t: &str| {
            Inline::Run(docxcore::model::Run {
                text: t.into(),
                ..Default::default()
            })
        };
        self.with_editor(window, cx, |e| {
            e.paste(&Clip {
                paras: vec![vec![
                    text("Page "),
                    field("PAGE"),
                    text(" of "),
                    field("NUMPAGES"),
                ]],
            })
        });
    }
}

#[cfg(test)]
mod tests;
