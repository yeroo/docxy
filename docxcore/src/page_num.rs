//! Page-number values shared by the PDF export and the suite's page view:
//! `w:pgNumType` formatting, the chapter prefix and the page-dependent fields
//! (`PAGE`, `NUMPAGES`, `SECTIONPAGES`), so both draw the same text.

use crate::sect::PageNumberFormat;

/// The largest `w:start` a page honours.
pub const MAX_PAGE_START: i32 = 32767;

/// `w:pgNumType w:fmt`; every format but the four roman/letter ones is decimal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageNumFmt {
    Decimal,
    LowerRoman,
    UpperRoman,
    LowerLetter,
    UpperLetter,
}

impl PageNumFmt {
    pub fn from_attr(fmt: Option<&str>) -> PageNumFmt {
        match fmt {
            Some("lowerRoman") => PageNumFmt::LowerRoman,
            Some("upperRoman") => PageNumFmt::UpperRoman,
            Some("lowerLetter") => PageNumFmt::LowerLetter,
            Some("upperLetter") => PageNumFmt::UpperLetter,
            _ => PageNumFmt::Decimal,
        }
    }
}

/// A `w:start`, as a page honours it: 0 to [`MAX_PAGE_START`].
pub fn clamp_start(start: i32) -> i32 {
    start.clamp(0, MAX_PAGE_START)
}

pub fn format_page_number(n: u32, fmt: PageNumFmt) -> String {
    match fmt {
        PageNumFmt::Decimal => n.to_string(),
        PageNumFmt::LowerRoman => roman(n).to_lowercase(),
        PageNumFmt::UpperRoman => roman(n),
        PageNumFmt::LowerLetter => letters(n).to_lowercase(),
        PageNumFmt::UpperLetter => letters(n),
    }
}

fn roman(mut n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, digits) in TABLE {
        while n >= value {
            out.push_str(digits);
            n -= value;
        }
    }
    out
}

/// Word's letter numbering: A..Z, then AA..ZZ, AAA.. (the letter repeats).
fn letters(n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let letter = char::from(b'A' + ((n - 1) % 26) as u8);
    std::iter::repeat_n(letter, ((n - 1) / 26 + 1) as usize).collect()
}

/// The heading level and separator a section's page numbers are prefixed with
/// (`w:chapStyle`, `w:chapSep`), if it has them.
pub fn chapter_prefix(format: &PageNumberFormat) -> Option<(u8, &'static str)> {
    let level = u8::try_from(format.chap_style?)
        .ok()
        .filter(|l| (1..=9).contains(l))?;
    let sep = match format.chap_sep.as_deref() {
        Some("period") => ".",
        Some("colon") => ":",
        Some("emDash") => "\u{2014}",
        Some("enDash") => "\u{2013}",
        _ => "-",
    };
    Some((level, sep))
}

/// Each page's chapter number. `pages` gives, per page in order, the heading
/// level its section prefixes with (`None` without `chapStyle`) and the
/// headings whose first line is on the page (level, number or `None` for an
/// unnumbered one). A page's chapter is the first heading of that level on it,
/// else the last one before it (STYLEREF's rule in a header or footer); `None`
/// without such a heading or when it is unnumbered, so the page shows its
/// plain number. The last heading of every level is carried across pages
/// whichever section they belong to.
pub fn page_chapters<'a>(
    pages: impl IntoIterator<Item = (Option<u8>, &'a [(u8, Option<String>)])>,
) -> Vec<Option<String>> {
    let mut last: [Option<&str>; 10] = Default::default();
    let mut out = Vec::new();
    for (level, headings) in pages {
        let chapter = level.and_then(|level| match headings.iter().find(|(l, _)| *l == level) {
            Some((_, text)) => text.as_deref(),
            None => last.get(usize::from(level)).copied().flatten(),
        });
        out.push(chapter.map(str::to_string));
        for (level, text) in headings {
            if let Some(slot) = last.get_mut(usize::from(*level)) {
                *slot = text.as_deref();
            }
        }
    }
    out
}

/// A page's PAGE text: its formatted number, prefixed by the chapter when the
/// section has `chapStyle` and the page has a numbered chapter.
pub fn page_text(
    number: u32,
    fmt: PageNumFmt,
    chapter: Option<&str>,
    prefix: Option<(u8, &str)>,
) -> String {
    let number = format_page_number(number, fmt);
    match (chapter, prefix) {
        (Some(chapter), Some((_, sep))) => format!("{chapter}{sep}{number}"),
        _ => number,
    }
}

/// A page-dependent field, substituted when the page is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageField {
    Page,
    NumPages,
    SectionPages,
}

pub fn page_field_kind(instr: &str) -> Option<PageField> {
    match instr
        .split_whitespace()
        .next()?
        .to_ascii_uppercase()
        .as_str()
    {
        "PAGE" => Some(PageField::Page),
        "NUMPAGES" => Some(PageField::NumPages),
        "SECTIONPAGES" => Some(PageField::SectionPages),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_page_number(3, PageNumFmt::LowerRoman), "iii");
        assert_eq!(format_page_number(14, PageNumFmt::UpperRoman), "XIV");
        assert_eq!(format_page_number(3, PageNumFmt::UpperLetter), "C");
        assert_eq!(format_page_number(28, PageNumFmt::UpperLetter), "BB");
        assert_eq!(format_page_number(3, PageNumFmt::LowerLetter), "c");
        assert_eq!(format_page_number(7, PageNumFmt::Decimal), "7");
        assert_eq!(format_page_number(0, PageNumFmt::LowerRoman), "0");
        assert_eq!(format_page_number(0, PageNumFmt::UpperLetter), "0");
        assert_eq!(
            PageNumFmt::from_attr(Some("numberInDash")),
            PageNumFmt::Decimal
        );
        assert_eq!(PageNumFmt::from_attr(None), PageNumFmt::Decimal);
    }

    #[test]
    fn start_clamps_like_a_page() {
        assert_eq!(clamp_start(-4), 0);
        assert_eq!(clamp_start(5), 5);
        assert_eq!(clamp_start(1_000_000), MAX_PAGE_START);
    }

    #[test]
    fn chapters_carry_and_fall_back() {
        let h1 = [(1u8, Some("1".to_string())), (1, Some("2".to_string()))];
        let none: [(u8, Option<String>); 0] = [];
        let unnumbered = [(1u8, None)];
        let sub = [(2u8, Some("2.1".to_string()))];
        let got = page_chapters([
            (Some(1), &h1[..1]),
            (Some(1), &none[..]),
            (Some(1), &h1[1..]),
            (Some(1), &sub[..]),
            (Some(1), &unnumbered[..]),
            (Some(1), &none[..]),
            (None, &h1[..]),
        ]);
        assert_eq!(
            got,
            vec![
                Some("1".into()),
                Some("1".into()),
                Some("2".into()),
                Some("2".into()),
                None,
                None,
                None,
            ]
        );
        assert_eq!(page_chapters([(Some(1), &none[..])]), vec![None]);
    }

    #[test]
    fn page_text_prefixes_only_with_both() {
        let hyphen = Some((1, "-"));
        assert_eq!(page_text(3, PageNumFmt::Decimal, Some("2"), hyphen), "2-3");
        assert_eq!(page_text(3, PageNumFmt::Decimal, None, hyphen), "3");
        assert_eq!(page_text(3, PageNumFmt::Decimal, Some("2"), None), "3");
        assert_eq!(
            page_text(3, PageNumFmt::LowerRoman, Some("2"), Some((1, "."))),
            "2.iii"
        );
    }

    #[test]
    fn field_kinds() {
        assert_eq!(
            page_field_kind(" PAGE \\* MERGEFORMAT"),
            Some(PageField::Page)
        );
        assert_eq!(page_field_kind("numpages"), Some(PageField::NumPages));
        assert_eq!(
            page_field_kind("SECTIONPAGES"),
            Some(PageField::SectionPages)
        );
        assert_eq!(page_field_kind("DATE"), None);
        assert_eq!(page_field_kind(""), None);
    }
}
