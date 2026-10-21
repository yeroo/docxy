//! Live PAGE / NUMPAGES / SECTIONPAGES values for the page view's headers and
//! footers (#1437): each page's own number, formatted by its section's
//! `w:pgNumType` (format, restart, chapter prefix), by the rule the PDF export
//! uses (`docxcore::page_num`, shared), so what a page shows is what a printout
//! of it would.

use crate::page_flow::PageFlow;
use docxcore::page_num::{PageField, page_chapters, page_text};

/// What the page-dependent fields show on one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageVals {
    pub page: String,
    pub num_pages: String,
    pub section_pages: String,
}

impl PageVals {
    pub(crate) fn value(&self, field: PageField) -> &str {
        match field {
            PageField::Page => &self.page,
            PageField::NumPages => &self.num_pages,
            PageField::SectionPages => &self.section_pages,
        }
    }
}

/// The values of every page of `pf`. `headings` are the body's headings as
/// `(top-level block, level, chapter number or None)` in document order
/// (`docxcore::numbering::heading_chapters`; empty when no section shows a
/// chapter). The page's owning section (`bands[0]`) decides its format, as
/// export.rs's `page.sect` does; SECTIONPAGES counts the pages a section has
/// a band on, fillers included (a filler belongs to the previous section).
/// A block is never split across pages, so a heading counts on the page its
/// block is in.
pub(crate) fn page_values(
    pf: &PageFlow,
    headings: &[(usize, u8, Option<String>)],
) -> Vec<PageVals> {
    let mut per_section = vec![0usize; pf.sections.len()];
    let mut on_page: Vec<Vec<(u8, Option<String>)>> = Vec::with_capacity(pf.pages.len());
    for page in &pf.pages {
        let mut seen: Vec<usize> = Vec::new();
        let mut found = Vec::new();
        for band in &page.bands {
            if !seen.contains(&band.section) {
                seen.push(band.section);
            }
            for &(s, e) in &band.cols {
                let from = headings.partition_point(|(b, ..)| *b < s);
                found.extend(
                    headings[from..]
                        .iter()
                        .take_while(|(b, ..)| *b < e)
                        .map(|(_, level, text)| (*level, text.clone())),
                );
            }
        }
        for s in seen {
            if let Some(n) = per_section.get_mut(s) {
                *n += 1;
            }
        }
        on_page.push(found);
    }
    let owner = |i: usize| &pf.sections[pf.pages[i].bands[0].section];
    let chapters = page_chapters(
        on_page
            .iter()
            .enumerate()
            .map(|(i, h)| (owner(i).chapter.map(|(level, _)| level), h.as_slice())),
    );
    let total = pf.pages.len().to_string();
    pf.pages
        .iter()
        .enumerate()
        .zip(chapters)
        .map(|((i, page), chapter)| {
            let sect = owner(i);
            PageVals {
                page: page_text(
                    u32::try_from(page.number.max(0)).unwrap_or(0),
                    sect.num_fmt,
                    chapter.as_deref(),
                    sect.chapter,
                ),
                num_pages: total.clone(),
                section_pages: per_section[page.bands[0].section].to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
