//! Header and footer model for the print layout and the header/footer
//! editing mode (#640, #641): which section and variant each page shows, and
//! which package part that resolves to.
//!
//! Resolution always reads the **body editor's** sectPrs (`Editor::sections`),
//! never the package's saved document: link changes, created references and
//! their undo live in the editor until Save.

use crate::{DocTab, HfEdit, Surface, parse_hf_part};
use docxcore::editor::Editor;
use docxcore::package::{HeaderVariant, Package, SectionParts, section_header_parts};
use docxcore::sect::{SectionSetup, has_flag, hf_reference, set_hf_reference};

/// What one page of the print layout shows in its margins: the section it
/// belongs to and the header/footer variant that section applies to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageSlot {
    pub section: usize,
    pub variant: HeaderVariant,
}

/// Every section's applied header and footer parts, from the editor's live
/// sectPrs and the package's document relationships.
pub(crate) fn resolve(ed: &Editor, pkg: &Package) -> Vec<SectionParts> {
    section_header_parts(&ed.sections(), &pkg.document_rels())
}

/// The page slot of each page, given each page's first body block. A page
/// belongs to the section of its first block; the section's first page shows
/// the first-page variant when the section has Different First Page
/// (`w:titlePg`), and with Different Odd & Even Pages every even page number
/// (counted through the whole document) shows the even variant.
pub(crate) fn page_slots(ed: &Editor, first_blocks: &[usize], even_odd: bool) -> Vec<PageSlot> {
    let sections = ed.sections();
    let mut out = Vec::with_capacity(first_blocks.len());
    let mut prev_section = None;
    for (pi, &block) in first_blocks.iter().enumerate() {
        let section = ed
            .section_of_block(block)
            .min(sections.len().saturating_sub(1));
        let first_of_section = prev_section != Some(section);
        prev_section = Some(section);
        let title_pg = sections
            .get(section)
            .is_some_and(|s| has_flag(s, "w:titlePg"));
        let variant = if first_of_section && title_pg {
            HeaderVariant::First
        } else if even_odd && (pi + 1).is_multiple_of(2) {
            HeaderVariant::Even
        } else {
            HeaderVariant::Default
        };
        out.push(PageSlot { section, variant });
    }
    out
}

/// The header (`is_header`) or footer part a page slot shows, if any.
pub(crate) fn slot_part(parts: &[SectionParts], slot: PageSlot, is_header: bool) -> Option<&str> {
    parts
        .get(slot.section)?
        .get(is_header, slot.variant)
        .map(|p| p.part_name.as_str())
}

/// The page on which the header/footer of `section`/`variant` is edited: the
/// first page showing that slot, else the section's first page (a variant no
/// page shows yet still gets a visible surface), else the first page.
pub(crate) fn edit_page(slots: &[PageSlot], section: usize, variant: HeaderVariant) -> usize {
    slots
        .iter()
        .position(|s| s.section == section && s.variant == variant)
        .or_else(|| slots.iter().position(|s| s.section == section))
        .unwrap_or(0)
}

/// The blank sheet an oddPage/evenPage section start inserts must not take a
/// section's First variant (`w:titlePg`): demote any a filler was given to
/// Default, or to Even under Different Odd & Even when its physical page is
/// even. Called on the slots both page-slot consumers build.
pub(crate) fn demote_filler_firsts(slots: &mut [PageSlot], fillers: &[bool], even_odd: bool) {
    for (pi, is_filler) in fillers.iter().copied().enumerate() {
        if !is_filler {
            continue;
        }
        if let Some(slot) = slots.get_mut(pi) {
            if slot.variant == HeaderVariant::First {
                slot.variant = if even_odd && (pi + 1).is_multiple_of(2) {
                    HeaderVariant::Even
                } else {
                    HeaderVariant::Default
                };
            }
        }
    }
}

/// The paragraph style new header (`Header`) or footer (`Footer`) content uses.
pub(crate) fn style_id(is_header: bool) -> &'static str {
    if is_header { "Header" } else { "Footer" }
}

/// One empty paragraph in the Header or Footer style: a new part's content.
pub(crate) fn empty_content(is_header: bool) -> String {
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"{}\"/></w:pPr></w:p>",
        style_id(is_header)
    )
}

/// The section a new header/footer of `variant` is referenced from when
/// `section` resolves to none: the nearest section at or before it that
/// carries its own (unresolvable) reference of that variant, whose link chain
/// `section` follows, else section 0. Referencing it there keeps every section
/// in between linked, as Word does (no reference is written into them).
fn chain_root(
    sections: &[String],
    section: usize,
    is_header: bool,
    variant: HeaderVariant,
) -> usize {
    (0..=section.min(sections.len().saturating_sub(1)))
        .rev()
        .find(|&k| hf_reference(&sections[k], is_header, variant.as_ooxml()).is_some())
        .unwrap_or(0)
}

/// Create a header (`is_header`) or footer part holding `content_xml` for
/// `section`/`variant` when that slot resolves to no part, and reference it
/// from the chain root (see [`chain_root`]) as one body-editor undo step.
/// Makes sure the Header/Footer styles exist. The new part's name.
pub(crate) fn create_for(
    tab: &mut DocTab,
    section: usize,
    is_header: bool,
    variant: HeaderVariant,
    content_xml: &str,
) -> Option<String> {
    let (Some(pkg), Surface::Doc(ed)) = (tab.pkg.as_mut(), &mut tab.surface) else {
        return None;
    };
    pkg.ensure_styles(&[style_id(is_header)]);
    let (rid, part_name) = pkg.create_hf_part(is_header, content_xml)?;
    let root = chain_root(&ed.sections(), section, is_header, variant);
    ed.edit_sections(&[root], |raw| {
        set_hf_reference(raw, is_header, variant.as_ooxml(), Some(&rid))
    });
    tab.set_dirty();
    Some(part_name)
}

/// Open the header (`is_header`) or footer of `section`/`variant` for editing:
/// the part that slot resolves to (its own, or the one it inherits: editing a
/// linked header edits the shared part, as in Word), else a new part created
/// by [`create_for`]. False, with the reason in the status, when the tab has
/// no package or the part could not be created.
pub(crate) fn open(
    tab: &mut DocTab,
    section: usize,
    is_header: bool,
    variant: HeaderVariant,
) -> bool {
    let (Some(pkg), Surface::Doc(ed)) = (tab.pkg.as_ref(), &tab.surface) else {
        tab.status = "Headers/footers need a .docx (not a Markdown document)".into();
        return false;
    };
    let section = section.min(ed.sections().len().saturating_sub(1));
    let resolved = resolve(ed, pkg)
        .get(section)
        .and_then(|p| p.get(is_header, variant))
        .map(|a| a.part_name.clone());
    let part_name = match resolved {
        Some(name) => name,
        None => match create_for(tab, section, is_header, variant, &empty_content(is_header)) {
            Some(name) => name,
            None => {
                tab.status = "Could not create the header/footer part".into();
                return false;
            }
        },
    };
    let Some(pkg) = tab.pkg.as_ref() else {
        return false;
    };
    let doc = docxcore::model::Document {
        body: parse_hf_part(pkg, &part_name),
    };
    tab.hf_edit = Some(HfEdit {
        editor: Editor::new(doc),
        part_name,
        is_header,
        section,
        variant,
        show_text: true,
    });
    if let Some(label) = crate::hf_tab::edit_label(tab) {
        tab.status = format!("Editing {label} \u{2014} press Esc to return to the document").into();
    }
    true
}

/// The header (`is_header`) or footer distance of a section, in twips
/// (`w:pgMar w:header` / `w:footer`; 720 when absent).
pub(crate) fn distance(sect: &str, is_header: bool) -> i32 {
    let m = SectionSetup::parse(sect).margins;
    if is_header { m.header } else { m.footer }
}

#[cfg(test)]
pub(crate) mod tests;
