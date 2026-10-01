//! The page-setup model of one worksheet.
//!
//! [`PageSetup`] gathers what four worksheet elements and one `sheetPr` child
//! say about printing: `<printOptions>`, `<pageMargins>`, `<pageSetup>`,
//! `<headerFooter>` and `<sheetPr><pageSetUpPr fitToPage>`. An absent
//! attribute reads as its schema default (ECMA-376 §18.3.1), which is also
//! what [`PageSetup::default`] holds. Margins are the exception: every
//! `<pageMargins>` attribute is required, so an absent element reads as the
//! margins Excel gives a new sheet.
//!
//! The model is read at load ([`crate::sheet::Sheet::page_setup_loaded`]
//! keeps the parse result) and written back by patching only the attributes
//! that changed, so anything it doesn't model (`r:id`, `horizontalDpi`,
//! `copies`, …) and every untouched spelling survives a save.

/// Page margins in inches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Margins {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    /// From the top edge of the paper to the header.
    pub header: f64,
    /// From the bottom edge of the paper to the footer.
    pub footer: f64,
}

impl Default for Margins {
    /// Excel's `Normal` margins, the ones a new sheet gets.
    fn default() -> Self {
        Margins {
            left: 0.7,
            right: 0.7,
            top: 0.75,
            bottom: 0.75,
            header: 0.3,
            footer: 0.3,
        }
    }
}

/// `pageSetup/@orientation`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    /// `default`: the printer's default, which prints portrait.
    #[default]
    Default,
    Portrait,
    Landscape,
}

impl Orientation {
    pub fn as_str(self) -> &'static str {
        match self {
            Orientation::Default => "default",
            Orientation::Portrait => "portrait",
            Orientation::Landscape => "landscape",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Orientation::Default),
            "portrait" => Some(Orientation::Portrait),
            "landscape" => Some(Orientation::Landscape),
            _ => None,
        }
    }

    /// Does the page print landscape?
    pub fn is_landscape(self) -> bool {
        self == Orientation::Landscape
    }
}

/// `pageSetup/@pageOrder`: the order pages of a sheet wider and taller than
/// one page print in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageOrder {
    #[default]
    DownThenOver,
    OverThenDown,
}

impl PageOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            PageOrder::DownThenOver => "downThenOver",
            PageOrder::OverThenDown => "overThenDown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "downThenOver" => Some(PageOrder::DownThenOver),
            "overThenDown" => Some(PageOrder::OverThenDown),
            _ => None,
        }
    }
}

/// `pageSetup/@cellComments`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellComments {
    #[default]
    None,
    AsDisplayed,
    AtEnd,
}

impl CellComments {
    pub fn as_str(self) -> &'static str {
        match self {
            CellComments::None => "none",
            CellComments::AsDisplayed => "asDisplayed",
            CellComments::AtEnd => "atEnd",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(CellComments::None),
            "asDisplayed" => Some(CellComments::AsDisplayed),
            "atEnd" => Some(CellComments::AtEnd),
            _ => None,
        }
    }
}

/// `pageSetup/@errors`: how error values print.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrintErrors {
    #[default]
    Displayed,
    Blank,
    Dash,
    NA,
}

impl PrintErrors {
    pub fn as_str(self) -> &'static str {
        match self {
            PrintErrors::Displayed => "displayed",
            PrintErrors::Blank => "blank",
            PrintErrors::Dash => "dash",
            PrintErrors::NA => "NA",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "displayed" => Some(PrintErrors::Displayed),
            "blank" => Some(PrintErrors::Blank),
            "dash" => Some(PrintErrors::Dash),
            "NA" => Some(PrintErrors::NA),
            _ => None,
        }
    }
}

/// Which header or footer string of a [`HeaderFooter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HfSlot {
    OddHeader,
    OddFooter,
    EvenHeader,
    EvenFooter,
    FirstHeader,
    FirstFooter,
}

impl HfSlot {
    /// Every slot, in the order `CT_HeaderFooter` (a sequence) requires its
    /// child elements.
    pub const ALL: [HfSlot; 6] = [
        HfSlot::OddHeader,
        HfSlot::OddFooter,
        HfSlot::EvenHeader,
        HfSlot::EvenFooter,
        HfSlot::FirstHeader,
        HfSlot::FirstFooter,
    ];

    /// The child element's local name.
    pub fn element(self) -> &'static str {
        match self {
            HfSlot::OddHeader => "oddHeader",
            HfSlot::OddFooter => "oddFooter",
            HfSlot::EvenHeader => "evenHeader",
            HfSlot::EvenFooter => "evenFooter",
            HfSlot::FirstHeader => "firstHeader",
            HfSlot::FirstFooter => "firstFooter",
        }
    }

    /// The slot for a child element's local name.
    pub fn from_element(name: &str) -> Option<Self> {
        HfSlot::ALL.into_iter().find(|s| s.element() == name)
    }

    /// The slot for a page kind (`odd`, `even`, `first`) and part (`header`,
    /// `footer`), as the control verbs name them.
    pub fn from_kind(kind: &str, part: &str) -> Option<Self> {
        Some(match (kind, part) {
            ("odd", "header") => HfSlot::OddHeader,
            ("odd", "footer") => HfSlot::OddFooter,
            ("even", "header") => HfSlot::EvenHeader,
            ("even", "footer") => HfSlot::EvenFooter,
            ("first", "header") => HfSlot::FirstHeader,
            ("first", "footer") => HfSlot::FirstFooter,
            _ => return None,
        })
    }

    pub fn is_header(self) -> bool {
        matches!(
            self,
            HfSlot::OddHeader | HfSlot::EvenHeader | HfSlot::FirstHeader
        )
    }
}

/// `<headerFooter>`: its flags and the six stored strings, in Excel's code
/// form (`&CPage &P of &N`). `None` is an absent child element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderFooter {
    pub different_odd_even: bool,
    pub different_first: bool,
    pub scale_with_doc: bool,
    pub align_with_margins: bool,
    pub odd_header: Option<String>,
    pub odd_footer: Option<String>,
    pub even_header: Option<String>,
    pub even_footer: Option<String>,
    pub first_header: Option<String>,
    pub first_footer: Option<String>,
}

impl Default for HeaderFooter {
    fn default() -> Self {
        HeaderFooter {
            different_odd_even: false,
            different_first: false,
            scale_with_doc: true,
            align_with_margins: true,
            odd_header: None,
            odd_footer: None,
            even_header: None,
            even_footer: None,
            first_header: None,
            first_footer: None,
        }
    }
}

impl HeaderFooter {
    pub fn get(&self, slot: HfSlot) -> Option<&str> {
        match slot {
            HfSlot::OddHeader => self.odd_header.as_deref(),
            HfSlot::OddFooter => self.odd_footer.as_deref(),
            HfSlot::EvenHeader => self.even_header.as_deref(),
            HfSlot::EvenFooter => self.even_footer.as_deref(),
            HfSlot::FirstHeader => self.first_header.as_deref(),
            HfSlot::FirstFooter => self.first_footer.as_deref(),
        }
    }

    pub fn slot_mut(&mut self, slot: HfSlot) -> &mut Option<String> {
        match slot {
            HfSlot::OddHeader => &mut self.odd_header,
            HfSlot::OddFooter => &mut self.odd_footer,
            HfSlot::EvenHeader => &mut self.even_header,
            HfSlot::EvenFooter => &mut self.even_footer,
            HfSlot::FirstHeader => &mut self.first_header,
            HfSlot::FirstFooter => &mut self.first_footer,
        }
    }

    /// The header (or footer) printed on page `n` (1-based within the sheet),
    /// as the flags choose it.
    pub fn for_page(&self, n: u32, header: bool) -> Option<&str> {
        let slot = if self.different_first && n == 1 {
            if header {
                HfSlot::FirstHeader
            } else {
                HfSlot::FirstFooter
            }
        } else if self.different_odd_even && n.is_multiple_of(2) {
            if header {
                HfSlot::EvenHeader
            } else {
                HfSlot::EvenFooter
            }
        } else if header {
            HfSlot::OddHeader
        } else {
            HfSlot::OddFooter
        };
        self.get(slot).filter(|s| !s.is_empty())
    }
}

/// The smallest and largest `Adjust to` percentage.
pub const SCALE_RANGE: std::ops::RangeInclusive<u32> = 10..=400;
/// The largest `Fit to` page count (0 is Automatic).
pub const FIT_MAX: u32 = 32767;

/// A worksheet's page setup. See the module docs for where each field lives.
#[derive(Clone, Debug, PartialEq)]
pub struct PageSetup {
    pub margins: Margins,
    /// `paperSize`: a code from ECMA-376 §18.3.1.63 (1 Letter, 9 A4, …).
    pub paper_size: u32,
    pub orientation: Orientation,
    /// `scale` (10–400): applies only while [`PageSetup::fit_to_page`] is off.
    pub scale: u32,
    /// `sheetPr/pageSetUpPr/@fitToPage`: scale to fit
    /// [`PageSetup::fit_width`] × [`PageSetup::fit_height`] pages.
    pub fit_to_page: bool,
    /// `fitToWidth`: pages wide, 0 = Automatic. The schema default is 1.
    pub fit_width: u32,
    /// `fitToHeight`: pages tall, 0 = Automatic. The schema default is 1.
    pub fit_height: u32,
    /// `firstPageNumber` while `useFirstPageNumber` is set; `None` is Auto
    /// (numbering starts at 1, or continues from the previous sheet).
    pub first_page_number: Option<u32>,
    pub page_order: PageOrder,
    pub black_and_white: bool,
    pub draft: bool,
    pub cell_comments: CellComments,
    pub errors: PrintErrors,
    /// `printOptions/@gridLines`.
    pub grid_lines: bool,
    /// `printOptions/@headings`: row and column headings.
    pub headings: bool,
    /// `printOptions/@horizontalCentered`.
    pub h_centered: bool,
    /// `printOptions/@verticalCentered`.
    pub v_centered: bool,
    pub header_footer: HeaderFooter,
}

impl Default for PageSetup {
    fn default() -> Self {
        PageSetup {
            margins: Margins::default(),
            paper_size: 1,
            orientation: Orientation::Default,
            scale: 100,
            fit_to_page: false,
            fit_width: 1,
            fit_height: 1,
            first_page_number: None,
            page_order: PageOrder::DownThenOver,
            black_and_white: false,
            draft: false,
            cell_comments: CellComments::None,
            errors: PrintErrors::Displayed,
            grid_lines: false,
            headings: false,
            h_centered: false,
            v_centered: false,
            header_footer: HeaderFooter::default(),
        }
    }
}

impl PageSetup {
    /// Refuse values Excel's Page Setup refuses: a scale outside 10–400, a
    /// fit count above 32767, a negative or non-finite margin, paper code 0,
    /// a first page number of 0. Header and footer sections are checked
    /// where they are typed ([`crate::print::hf::Sections::from_editor`]).
    pub fn validate(&self) -> Result<(), String> {
        if !SCALE_RANGE.contains(&self.scale) {
            return Err(format!(
                "scale {} is outside {}–{}",
                self.scale,
                SCALE_RANGE.start(),
                SCALE_RANGE.end()
            ));
        }
        for (name, n) in [
            ("fitToWidth", self.fit_width),
            ("fitToHeight", self.fit_height),
        ] {
            if n > FIT_MAX {
                return Err(format!("{name} {n} is outside 0–{FIT_MAX}"));
            }
        }
        let m = &self.margins;
        for (name, v) in [
            ("left", m.left),
            ("right", m.right),
            ("top", m.top),
            ("bottom", m.bottom),
            ("header", m.header),
            ("footer", m.footer),
        ] {
            if !v.is_finite() || v < 0.0 {
                return Err(format!("{name} margin {v} must be a number of inches ≥ 0"));
            }
        }
        if self.paper_size == 0 {
            return Err("paperSize must be a paper code ≥ 1".into());
        }
        if let Some(0) = self.first_page_number {
            return Err("firstPageNumber must be ≥ 1".into());
        }
        Ok(())
    }

    /// The paper's (width, height) in points as it prints: the paper size,
    /// turned for landscape.
    pub fn page_points(&self) -> (f64, f64) {
        let (w, h) = paper_points(self.paper_size);
        if self.orientation.is_landscape() {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// The page setup a grouped sheet takes from this one when Page Setup
    /// is applied to the group (FIL-147): every field, and the header and
    /// footer text without picture codes (`&G`), since the pictures
    /// (`legacyDrawingHF`) belong to this sheet. Print areas and titles are
    /// defined names, not page setup, so they never travel.
    pub fn for_group(&self) -> PageSetup {
        let mut s = self.clone();
        for slot in HfSlot::ALL {
            if let Some(t) = s.header_footer.slot_mut(slot) {
                *t = crate::print::hf::strip_pictures(t);
            }
        }
        s
    }
}

/// The (width, height) in points of a `paperSize` code. Codes this table
/// doesn't know print on Letter, as Excel does with a paper the printer
/// lacks.
pub fn paper_points(code: u32) -> (f64, f64) {
    // (inches) or (millimetres) per ECMA-376 §18.3.1.63.
    const MM: f64 = 72.0 / 25.4;
    match code {
        1 | 2 => (8.5 * 72.0, 11.0 * 72.0), // Letter, Letter small
        3 => (11.0 * 72.0, 17.0 * 72.0),    // Tabloid
        4 => (17.0 * 72.0, 11.0 * 72.0),    // Ledger
        5 => (8.5 * 72.0, 14.0 * 72.0),     // Legal
        6 => (5.5 * 72.0, 8.5 * 72.0),      // Statement
        7 => (7.25 * 72.0, 10.5 * 72.0),    // Executive
        8 => (297.0 * MM, 420.0 * MM),      // A3
        9 | 10 => (210.0 * MM, 297.0 * MM), // A4, A4 small
        11 => (148.0 * MM, 210.0 * MM),     // A5
        12 => (250.0 * MM, 353.0 * MM),     // B4 (JIS)
        13 => (182.0 * MM, 257.0 * MM),     // B5 (JIS)
        14 => (8.5 * 72.0, 13.0 * 72.0),    // Folio
        15 => (215.0 * MM, 275.0 * MM),     // Quarto
        16 => (10.0 * 72.0, 14.0 * 72.0),   // 10x14
        17 => (11.0 * 72.0, 17.0 * 72.0),   // 11x17
        18 => (8.5 * 72.0, 11.0 * 72.0),    // Note
        20 => (4.125 * 72.0, 9.5 * 72.0),   // #10 envelope
        27 => (110.0 * MM, 220.0 * MM),     // DL envelope
        28 => (162.0 * MM, 229.0 * MM),     // C5 envelope
        34 => (176.0 * MM, 250.0 * MM),     // B5 envelope
        37 => (3.875 * 72.0, 7.5 * 72.0),   // Monarch envelope
        66 => (420.0 * MM, 594.0 * MM),     // A2
        70 => (105.0 * MM, 148.0 * MM),     // A6
        _ => (8.5 * 72.0, 11.0 * 72.0),
    }
}

/// An XML boolean attribute value as the schema reads it.
pub(crate) fn xml_bool(v: &str) -> Option<bool> {
    match v.trim() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_schema_defaults() {
        let d = PageSetup::default();
        assert_eq!((d.fit_width, d.fit_height), (1, 1));
        assert_eq!(d.scale, 100);
        assert_eq!(d.paper_size, 1);
        assert_eq!(d.orientation, Orientation::Default);
        assert_eq!(d.page_order, PageOrder::DownThenOver);
        assert!(d.header_footer.scale_with_doc && d.header_footer.align_with_margins);
        assert_eq!(d.margins, Margins::default());
    }

    #[test]
    fn validate_refuses_scale_and_fit_out_of_range() {
        let mut s = PageSetup::default();
        for bad in [9, 401] {
            s.scale = bad;
            assert!(s.validate().is_err(), "{bad}");
        }
        for good in [10, 400] {
            s.scale = good;
            assert!(s.validate().is_ok(), "{good}");
        }
        s.fit_width = 32768;
        assert!(s.validate().is_err());
        s.fit_width = 0;
        s.fit_height = 32767;
        assert!(s.validate().is_ok());
        s.margins.left = -0.1;
        assert!(s.validate().is_err());
    }

    #[test]
    fn landscape_turns_the_paper() {
        let mut s = PageSetup {
            paper_size: 9,
            ..PageSetup::default()
        };
        let (w, h) = s.page_points();
        assert!(
            (w - 595.27).abs() < 0.1 && (h - 841.89).abs() < 0.1,
            "{w} {h}"
        );
        s.orientation = Orientation::Landscape;
        assert_eq!(s.page_points(), (h, w));
        assert_eq!(paper_points(999), paper_points(1));
    }

    #[test]
    fn a_group_copy_keeps_every_field_but_drops_header_pictures() {
        let mut s = PageSetup {
            orientation: Orientation::Landscape,
            scale: 75,
            grid_lines: true,
            ..PageSetup::default()
        };
        s.header_footer.odd_header = Some("&L&G&CTitle".into());
        s.header_footer.odd_footer = Some("&RPage &P".into());
        let copy = s.for_group();
        assert_eq!(copy.header_footer.odd_header.as_deref(), Some("&L&CTitle"));
        assert_eq!(copy.header_footer.odd_footer.as_deref(), Some("&RPage &P"));
        assert_eq!(
            PageSetup {
                header_footer: s.header_footer.clone(),
                ..copy
            },
            s
        );
    }

    #[test]
    fn header_for_page_follows_first_and_odd_even_flags() {
        let hf = HeaderFooter {
            different_first: true,
            different_odd_even: true,
            odd_header: Some("odd".into()),
            even_header: Some("even".into()),
            first_header: Some("first".into()),
            ..HeaderFooter::default()
        };
        assert_eq!(hf.for_page(1, true), Some("first"));
        assert_eq!(hf.for_page(2, true), Some("even"));
        assert_eq!(hf.for_page(3, true), Some("odd"));
        assert_eq!(hf.for_page(3, false), None);
    }
}
