//! The theme's fonts (`word/theme/theme1.xml`, `a:fontScheme`), which a
//! `w:rFonts w:asciiTheme="minorHAnsi"` names instead of a family (#1972).
//! Only the Latin typefaces are read: the East Asian and complex-script slots
//! (`a:ea`, `a:cs`) are not modelled.
//!
//! And the theme's colours (`a:clrScheme`), which a `w:themeColor="accent1"`
//! names instead of a value (#2231: a paragraph border's colour).

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

/// A theme colour a WordprocessingML `w:themeColor` names (ST_ThemeColor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColor {
    Dark1,
    Light1,
    Dark2,
    Light2,
    Accent1,
    Accent2,
    Accent3,
    Accent4,
    Accent5,
    Accent6,
    Hyperlink,
    FollowedHyperlink,
    /// `text1`: the scheme's `dk1` under Word's default colour mapping.
    Text1,
    /// `background1`: `lt1`.
    Background1,
    /// `text2`: `dk2`.
    Text2,
    /// `background2`: `lt2`.
    Background2,
}

impl ThemeColor {
    /// The `w:themeColor` value's colour; `None` for `none` or an unknown name.
    pub fn from_val(val: &str) -> Option<ThemeColor> {
        use ThemeColor::*;
        Some(match val {
            "dark1" => Dark1,
            "light1" => Light1,
            "dark2" => Dark2,
            "light2" => Light2,
            "accent1" => Accent1,
            "accent2" => Accent2,
            "accent3" => Accent3,
            "accent4" => Accent4,
            "accent5" => Accent5,
            "accent6" => Accent6,
            "hyperlink" => Hyperlink,
            "followedHyperlink" => FollowedHyperlink,
            "text1" => Text1,
            "background1" => Background1,
            "text2" => Text2,
            "background2" => Background2,
            _ => return None,
        })
    }

    /// The `w:themeColor` value that names it.
    pub fn val(self) -> &'static str {
        use ThemeColor::*;
        match self {
            Dark1 => "dark1",
            Light1 => "light1",
            Dark2 => "dark2",
            Light2 => "light2",
            Accent1 => "accent1",
            Accent2 => "accent2",
            Accent3 => "accent3",
            Accent4 => "accent4",
            Accent5 => "accent5",
            Accent6 => "accent6",
            Hyperlink => "hyperlink",
            FollowedHyperlink => "followedHyperlink",
            Text1 => "text1",
            Background1 => "background1",
            Text2 => "text2",
            Background2 => "background2",
        }
    }

    /// The scheme slot it reads (`text1` is `dk1`, and so on).
    fn slot(self) -> usize {
        use ThemeColor::*;
        match self {
            Dark1 | Text1 => 0,
            Light1 | Background1 => 1,
            Dark2 | Text2 => 2,
            Light2 | Background2 => 3,
            Accent1 => 4,
            Accent2 => 5,
            Accent3 => 6,
            Accent4 => 7,
            Accent5 => 8,
            Accent6 => 9,
            Hyperlink => 10,
            FollowedHyperlink => 11,
        }
    }
}

/// A theme colour as an attribute set names it: `w:themeColor`, with
/// `w:themeTint` / `w:themeShade` (`00`–`FF`) when given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColorRef {
    pub name: ThemeColor,
    pub tint: Option<u8>,
    pub shade: Option<u8>,
}

/// The scheme's colours, `0xRRGGBB`, by slot: dk1, lt1, dk2, lt2,
/// accent1–6, hlink, folHlink.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeColors {
    slots: [Option<u32>; 12],
}

impl ThemeColors {
    /// The colour `r` names, tinted or shaded, or `None` when the scheme
    /// lacks it. Tint and shade are applied to each RGB channel (`t`, `s` =
    /// value / 255): a tint `c × t + 255 × (1 − t)`, a shade `c × s`. Word
    /// works in HSL there (ECMA-376 17.3.2.41); the RGB form is the usual
    /// close approximation and leaves an untinted colour exact.
    pub fn resolve(&self, r: ThemeColorRef) -> Option<u32> {
        let base = self.slots[r.name.slot()]?;
        let mut ch = [(base >> 16) & 0xFF, (base >> 8) & 0xFF, base & 0xFF].map(|c| c as f32);
        if let Some(t) = r.tint {
            let t = f32::from(t) / 255.0;
            ch = ch.map(|c| c * t + 255.0 * (1.0 - t));
        }
        if let Some(s) = r.shade {
            let s = f32::from(s) / 255.0;
            ch = ch.map(|c| c * s);
        }
        let [r, g, b] = ch.map(|c| c.round().clamp(0.0, 255.0) as u32);
        Some((r << 16) | (g << 8) | b)
    }
}

fn colour_slot(name: &str) -> Option<usize> {
    Some(match name {
        "dk1" => 0,
        "lt1" => 1,
        "dk2" => 2,
        "lt2" => 3,
        "accent1" => 4,
        "accent2" => 5,
        "accent3" => 6,
        "accent4" => 7,
        "accent5" => 8,
        "accent6" => 9,
        "hlink" => 10,
        "folHlink" => 11,
        _ => return None,
    })
}

/// `RRGGBB` hex, `None` unless exactly six hex digits.
fn hex6(v: &str) -> Option<u32> {
    let v = v.trim();
    (v.len() == 6)
        .then(|| u32::from_str_radix(v, 16).ok())
        .flatten()
}

/// Reads `a:clrScheme`'s twelve colours, each an `a:srgbClr@val` or an
/// `a:sysClr@lastClr`; a slot with neither (or a bad value) is unset.
pub fn parse_theme_colors(xml: &str) -> ThemeColors {
    let mut out = ThemeColors::default();
    let mut p = XmlParser::new(xml);
    let mut in_scheme = false;
    let mut slot: Option<usize> = None;
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "clrScheme" => in_scheme = true,
                name if in_scheme && slot.is_none() => slot = colour_slot(name),
                "srgbClr" => {
                    if let Some(i) = slot {
                        out.slots[i] = out.slots[i].or(hex6(p.attr("val")));
                    }
                }
                "sysClr" => {
                    if let Some(i) = slot {
                        out.slots[i] = out.slots[i].or(hex6(p.attr("lastClr")));
                    }
                }
                _ => {}
            },
            Event::End => match local(p.name()) {
                "clrScheme" => {
                    in_scheme = false;
                    slot = None;
                }
                name if slot.is_some() && colour_slot(name) == slot => slot = None,
                _ => {}
            },
            Event::Eof => break,
            Event::Text => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLOURS: &str = r#"<a:theme xmlns:a="x"><a:themeElements><a:clrScheme name="Office">
<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
<a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
<a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2>
<a:accent1><a:srgbClr val="4F81BD"/></a:accent1><a:accent2><a:srgbClr val="C0504D"/></a:accent2>
<a:accent3><a:srgbClr val="zz"/></a:accent3>
<a:hlink><a:srgbClr val="0000FF"/></a:hlink></a:clrScheme></a:themeElements></a:theme>"#;

    fn r(name: ThemeColor) -> ThemeColorRef {
        ThemeColorRef {
            name,
            tint: None,
            shade: None,
        }
    }

    #[test]
    fn reads_the_colour_scheme_and_maps_text_and_background() {
        let c = parse_theme_colors(COLOURS);
        assert_eq!(c.resolve(r(ThemeColor::Accent1)), Some(0x4F81BD));
        assert_eq!(c.resolve(r(ThemeColor::Accent2)), Some(0xC0504D));
        assert_eq!(c.resolve(r(ThemeColor::Text1)), Some(0x000000));
        assert_eq!(c.resolve(r(ThemeColor::Background1)), Some(0xFFFFFF));
        assert_eq!(c.resolve(r(ThemeColor::Text2)), Some(0x1F497D));
        assert_eq!(c.resolve(r(ThemeColor::Background2)), Some(0xEEECE1));
        assert_eq!(c.resolve(r(ThemeColor::Hyperlink)), Some(0x0000FF));
        // A bad value and a missing slot are unset.
        assert_eq!(c.resolve(r(ThemeColor::Accent3)), None);
        assert_eq!(c.resolve(r(ThemeColor::Accent6)), None);
        assert_eq!(parse_theme_colors(""), ThemeColors::default());
    }

    #[test]
    fn tint_and_shade_move_toward_white_and_black() {
        let c = parse_theme_colors(COLOURS);
        let accent = |tint, shade| {
            c.resolve(ThemeColorRef {
                name: ThemeColor::Accent1,
                tint,
                shade,
            })
        };
        // 0x99 / 255 = 0.6: 0x4F → 79×0.6 + 102 = 149.4.
        assert_eq!(accent(Some(0x99), None), Some(0x95B3D7));
        // 0xBF / 255: 79 × 0.749 = 59.2.
        assert_eq!(accent(None, Some(0xBF)), Some(0x3B618E));
        assert_eq!(accent(Some(0xFF), None), Some(0x4F81BD));
        assert_eq!(accent(None, Some(0)), Some(0));
    }

    #[test]
    fn theme_color_names_round_trip() {
        for v in [
            "accent1",
            "text1",
            "background2",
            "followedHyperlink",
            "dark2",
        ] {
            assert_eq!(ThemeColor::from_val(v).map(ThemeColor::val), Some(v));
        }
        assert_eq!(ThemeColor::from_val("none"), None);
    }

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
