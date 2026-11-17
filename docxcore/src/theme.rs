//! The theme's fonts (`word/theme/theme1.xml`, `a:fontScheme`), which a
//! `w:rFonts w:asciiTheme="minorHAnsi"` names instead of a family (#1972).
//! Only the Latin typefaces are read: the East Asian and complex-script slots
//! (`a:ea`, `a:cs`) are not modelled.

use crate::xml::{Event, XmlParser};

/// The theme's major (headings) and minor (body) Latin families.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeFonts {
    pub major: Option<String>,
    pub minor: Option<String>,
}

impl ThemeFonts {
    /// The family a theme font reference names: `majorAscii`, `majorHAnsi`,
    /// `majorEastAsia`, `majorBidi` the major font, the `minor*` ones the
    /// minor font. `None` for any other name or a font the theme lacks.
    pub fn family(&self, reference: &str) -> Option<&str> {
        if reference.starts_with("major") {
            self.major.as_deref()
        } else if reference.starts_with("minor") {
            self.minor.as_deref()
        } else {
            None
        }
    }
}

/// An element's local name, its prefix dropped (`a:latin` → `latin`).
fn local(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

/// Reads `a:fontScheme`'s `a:majorFont/a:latin@typeface` and
/// `a:minorFont/a:latin@typeface`; an empty typeface is `None`.
pub fn parse_theme_fonts(xml: &str) -> ThemeFonts {
    let mut out = ThemeFonts::default();
    let mut p = XmlParser::new(xml);
    // Which of the font scheme's fonts the parser is in.
    let mut slot: Option<bool> = None;
    let mut depth = 0usize;
    let mut slot_depth = 0usize;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                match local(p.name()) {
                    "majorFont" => {
                        slot = Some(true);
                        slot_depth = depth;
                    }
                    "minorFont" => {
                        slot = Some(false);
                        slot_depth = depth;
                    }
                    "latin" if slot.is_some() && depth == slot_depth + 1 => {
                        let face = p.attr("typeface").trim();
                        let face = (!face.is_empty()).then(|| face.to_string());
                        if slot == Some(true) {
                            out.major = out.major.take().or(face);
                        } else {
                            out.minor = out.minor.take().or(face);
                        }
                    }
                    _ => {}
                }
            }
            Event::End => {
                if slot.is_some() && depth == slot_depth {
                    slot = None;
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme">
<a:themeElements><a:fontScheme name="Office">
<a:majorFont><a:latin typeface="Calibri Light" panose="020F0302020204030204"/>
<a:ea typeface=""/><a:cs typeface=""/><a:font script="Jpan" typeface="Yu Gothic Light"/></a:majorFont>
<a:minorFont><a:latin typeface="Calibri" panose="020F0502020204030204"/>
<a:ea typeface=""/><a:cs typeface=""/></a:minorFont>
</a:fontScheme></a:themeElements></a:theme>"#;

    #[test]
    fn reads_the_major_and_minor_latin_typefaces() {
        let t = parse_theme_fonts(THEME);
        assert_eq!(t.major.as_deref(), Some("Calibri Light"));
        assert_eq!(t.minor.as_deref(), Some("Calibri"));
        assert_eq!(t.family("minorHAnsi"), Some("Calibri"));
        assert_eq!(t.family("minorAscii"), Some("Calibri"));
        assert_eq!(t.family("majorHAnsi"), Some("Calibri Light"));
        assert_eq!(t.family("majorBidi"), Some("Calibri Light"));
        assert_eq!(t.family("other"), None);
    }

    #[test]
    fn an_empty_or_missing_typeface_is_none() {
        let t = parse_theme_fonts(
            r#"<a:theme xmlns:a="x"><a:themeElements><a:fontScheme>
            <a:majorFont><a:latin typeface=""/></a:majorFont></a:fontScheme></a:themeElements></a:theme>"#,
        );
        assert_eq!(t, ThemeFonts::default());
        assert_eq!(parse_theme_fonts(""), ThemeFonts::default());
    }
}
