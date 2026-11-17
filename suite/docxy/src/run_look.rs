//! How the document view draws a run (#1427): its direct formatting over the
//! character style, the paragraph style and the table style around it, as
//! Word lays them (`StyleSheet::shown_run`). A run that none of them gives a
//! family is drawn in the document's own font (`StyleSheet::doc_font`, theme
//! fonts resolved: #1972).

use docxcore::model::{Run, RunProps};
use docxcore::package::Package;
use docxcore::styles::{StyleSheet, parse_styles_xml};
use docxcore::table_styles::RunLook;
use docxcore::theme::parse_theme_fonts;
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

/// What lies below a paragraph's runs: the package's styles, the table
/// cell's run formatting and the paragraph's own style. All `None` draws a
/// run as it is.
#[derive(Clone, Copy, Default)]
pub struct RunStyler<'a> {
    pub sheet: Option<&'a StyleSheet>,
    pub cell: Option<&'a RunLook>,
    pub pstyle: Option<&'a str>,
}

impl<'a> RunStyler<'a> {
    /// The same styles for the paragraph styled `pstyle`.
    pub fn for_paragraph(self, pstyle: Option<&'a str>) -> Self {
        RunStyler { pstyle, ..self }
    }

    /// `r` as drawn: its styles below it, and the document's font
    /// ([`StyleSheet::doc_font`]) when none of them names a family (#1972).
    pub fn shown<'r>(&self, r: &'r Run) -> Cow<'r, RunProps> {
        let shown = self.styled(r);
        match self.sheet.and_then(StyleSheet::doc_font) {
            Some(family) if shown.font.is_none() => {
                let mut props = shown.into_owned();
                props.font = Some(family.to_string());
                Cow::Owned(props)
            }
            _ => shown,
        }
    }

    fn styled<'r>(&self, r: &'r Run) -> Cow<'r, RunProps> {
        match self.sheet {
            // Nothing lies below it: the run is its own.
            Some(_)
                if self.cell.is_none() && self.pstyle.is_none() && r.props.style_id.is_none() =>
            {
                Cow::Borrowed(&r.props)
            }
            Some(sheet) => Cow::Owned(sheet.shown_run(
                self.cell,
                self.pstyle,
                r.props.style_id.as_deref(),
                &r.props,
            )),
            None => match self.cell {
                Some(look) => Cow::Owned(look.under(&r.props)),
                None => Cow::Borrowed(&r.props),
            },
        }
    }
}

/// A run's text size in px at `zoom`: its own or its styles' `w:sz` (half
/// points, 96 dpi), else `base` (the paragraph's, already zoomed). The one
/// conversion drawing, tab widths and inline widths share (#1427).
pub fn run_size_px(props: &RunProps, base: f32, zoom: f32) -> f32 {
    props
        .size_half_pts
        .map_or(base, |h| zoom * h as f32 / 2.0 * 1.333)
}

/// A tab's parsed `word/styles.xml`, its theme fonts applied (#1972), kept
/// across frames. It is read again only when the package's parts moved
/// ([`Package::parts_stamp`]) and the styles or the theme part is not the
/// text already held, so a body edit parses nothing.
#[derive(Default)]
pub struct Cache {
    stamp: Option<u64>,
    xml: Option<Arc<str>>,
    theme: Option<Arc<str>>,
    sheet: Option<Rc<StyleSheet>>,
    /// How often the part was parsed.
    #[cfg(test)]
    parses: usize,
}

impl Cache {
    /// The styles text and sheet of `pkg`; `None`s without a package or part.
    pub fn current(&mut self, pkg: Option<&Package>) -> (Option<Arc<str>>, Option<Rc<StyleSheet>>) {
        let Some(pkg) = pkg else {
            self.stamp = None;
            self.xml = None;
            self.theme = None;
            self.sheet = None;
            return (None, None);
        };
        let stamp = pkg.parts_stamp();
        if self.stamp != Some(stamp) {
            self.stamp = Some(stamp);
            let text = pkg
                .part("word/styles.xml")
                .and_then(|b| std::str::from_utf8(b).ok());
            let theme_name = pkg.theme_part_name();
            let theme = theme_name
                .as_deref()
                .and_then(|n| pkg.part(n))
                .and_then(|b| std::str::from_utf8(b).ok());
            if self.xml.as_deref() != text || self.theme.as_deref() != theme {
                self.xml = text.map(Arc::from);
                self.theme = theme.map(Arc::from);
                #[cfg(test)]
                if text.is_some() {
                    self.parses += 1;
                }
                self.sheet = text.map(|t| {
                    let mut sheet = parse_styles_xml(t);
                    if let Some(theme) = theme {
                        sheet.apply_theme_fonts(&parse_theme_fonts(theme));
                    }
                    Rc::new(sheet)
                });
            }
        }
        (self.xml.clone(), self.sheet.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::Document;
    use docxcore::package::new_package;

    const STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:style w:type="paragraph" w:styleId="Heading1"><w:rPr><w:b/><w:sz w:val="32"/>
        <w:color w:val="1F3864"/><w:rFonts w:ascii="Georgia"/></w:rPr></w:style>
        <w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style></w:styles>"#;

    fn run(style: Option<&str>) -> Run {
        Run {
            text: "x".into(),
            props: RunProps {
                style_id: style.map(String::from),
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_heading_styled_run_shows_the_styles_bold_size_colour_and_font() {
        let sheet = parse_styles_xml(STYLES);
        let styler = RunStyler {
            sheet: Some(&sheet),
            ..Default::default()
        }
        .for_paragraph(Some("Heading1"));
        let plain_run = run(None);
        let shown = styler.shown(&plain_run);
        assert!(shown.bold);
        assert_eq!(shown.size_half_pts, Some(32));
        assert_eq!(shown.color.as_deref(), Some("1F3864"));
        assert_eq!(shown.font.as_deref(), Some("Georgia"));
        // A character style on a plain paragraph.
        let plain = RunStyler {
            sheet: Some(&sheet),
            ..Default::default()
        };
        let strong = run(Some("Strong"));
        assert!(plain.shown(&strong).bold);
        assert!(!plain.shown(&plain_run).bold, "no style, no change");
        assert!(matches!(plain.shown(&plain_run), Cow::Borrowed(_)));
    }

    #[test]
    fn a_runs_size_scales_with_zoom_and_falls_back_to_the_paragraphs() {
        let styled = RunProps {
            size_half_pts: Some(32),
            ..Default::default()
        };
        assert!((run_size_px(&styled, 14.5, 1.0) - 21.328).abs() < 1e-3);
        assert!((run_size_px(&styled, 29.0, 2.0) - 42.656).abs() < 1e-3);
        assert_eq!(run_size_px(&RunProps::default(), 29.0, 2.0), 29.0);
    }

    #[test]
    fn without_a_sheet_the_cell_look_lays_below_as_before() {
        let look = RunLook {
            bold: Some(true),
            ..Default::default()
        };
        let styler = RunStyler {
            cell: Some(&look),
            ..Default::default()
        };
        let r = run(None);
        assert!(styler.shown(&r).bold);
        assert!(!RunStyler::default().shown(&r).bold);
    }

    #[test]
    fn a_body_edit_does_not_parse_the_styles_again() {
        let mut pkg = new_package(Document { body: vec![] });
        pkg.set_part("word/styles.xml", STYLES.as_bytes().to_vec());
        let mut cache = Cache::default();
        let (xml, sheet) = cache.current(Some(&pkg));
        assert!(xml.is_some() && sheet.is_some());
        assert_eq!(cache.parses, 1);
        cache.current(Some(&pkg));
        assert_eq!(cache.parses, 1, "same stamp");
        // Another part moves the stamp, the styles text is the same.
        pkg.set_part("word/other.xml", b"<x/>".to_vec());
        cache.current(Some(&pkg));
        assert_eq!(cache.parses, 1, "stamp moved, styles part unchanged");
        pkg.set_part("word/styles.xml", [STYLES, " "].concat().into_bytes());
        cache.current(Some(&pkg));
        assert_eq!(cache.parses, 2, "styles changed");
    }

    /// The owner's file (#1972): runs with only `w:b` in a paragraph with
    /// no style, under `docDefaults` naming the theme's minor font.
    #[test]
    fn a_run_naming_no_font_is_drawn_in_the_documents_theme_font_1972() {
        let mut pkg =
            docxcore::package::load_package(&crate::ext_fixture::theme_font_docx()).expect("loads");
        let mut cache = Cache::default();
        let sheet = cache.current(Some(&pkg)).1.expect("a sheet");
        let styler = RunStyler {
            sheet: Some(&sheet),
            ..Default::default()
        };
        let bold = Run {
            text: "Strong".into(),
            props: RunProps {
                bold: true,
                ..Default::default()
            },
        };
        let shown = styler.shown(&bold);
        assert!(shown.bold);
        assert_eq!(shown.font.as_deref(), Some("Calibri"));
        // A run's own family is kept.
        let named = Run {
            text: "x".into(),
            props: RunProps {
                font: Some("Georgia".into()),
                ..Default::default()
            },
        };
        assert!(matches!(styler.shown(&named), Cow::Borrowed(_)));
        assert_eq!(styler.shown(&named).font.as_deref(), Some("Georgia"));
        // A paragraph style naming no font still draws the document's.
        let styled = styler.for_paragraph(Some("Normal"));
        assert_eq!(styled.shown(&bold).font.as_deref(), Some("Calibri"));
        // Another theme is read again, the styles text unchanged.
        let parses = cache.parses;
        assert!(
            pkg.set_part(
                "word/theme/theme1.xml",
                crate::ext_fixture::THEME_FONT_THEME
                    .replace("\"Calibri\"", "\"Cambria\"")
                    .into_bytes(),
            )
        );
        let sheet = cache.current(Some(&pkg)).1.expect("a sheet");
        assert_eq!(cache.parses, parses + 1, "the theme changed");
        assert_eq!(sheet.doc_font(), Some("Cambria"));
        // Without the theme part the reference names nothing: today's look.
        let mut bare = new_package(Document { body: vec![] });
        bare.set_part(
            "word/styles.xml",
            crate::ext_fixture::THEME_FONT_STYLES.as_bytes().to_vec(),
        );
        let sheet = Cache::default().current(Some(&bare)).1.expect("a sheet");
        let styler = RunStyler {
            sheet: Some(&sheet),
            ..Default::default()
        };
        assert_eq!(styler.shown(&bold).font, None);
        assert!(matches!(styler.shown(&bold), Cow::Borrowed(_)));
    }
}
