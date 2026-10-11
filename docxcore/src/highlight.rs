//! Word's highlight colours (`w:highlight`, `ST_HighlightColor`): the one
//! name → RGB table, and the nearest name for a colour another app wrote.

/// Each highlight name and its RGB.
const HIGHLIGHTS: &[(&str, (u8, u8, u8))] = &[
    ("yellow", (0xFF, 0xFF, 0x00)),
    ("green", (0x00, 0xFF, 0x00)),
    ("cyan", (0x00, 0xFF, 0xFF)),
    ("magenta", (0xFF, 0x00, 0xFF)),
    ("blue", (0x00, 0x00, 0xFF)),
    ("red", (0xFF, 0x00, 0x00)),
    ("darkYellow", (0x80, 0x80, 0x00)),
    ("darkGreen", (0x00, 0x80, 0x00)),
    ("darkCyan", (0x00, 0x80, 0x80)),
    ("darkMagenta", (0x80, 0x00, 0x80)),
    ("darkBlue", (0x00, 0x00, 0x80)),
    ("darkRed", (0x80, 0x00, 0x00)),
    ("lightGray", (0xC0, 0xC0, 0xC0)),
    ("darkGray", (0x80, 0x80, 0x80)),
    ("black", (0x00, 0x00, 0x00)),
    ("white", (0xFF, 0xFF, 0xFF)),
];

/// The RGB of highlight `name`; `None` for `none` or a name Word does not
/// define.
pub fn highlight_rgb(name: &str) -> Option<(u8, u8, u8)> {
    HIGHLIGHTS.iter().find(|(n, _)| *n == name).map(|&(_, c)| c)
}

/// The highlight name nearest `rgb`, by hue and brightness rather than RGB
/// distance: another app's pale highlight (Pages' `FFFC9E`) is yellow, not
/// the white or light gray it is nearest in RGB. A colour with little
/// chroma is a gray by its brightness.
pub fn nearest_highlight((r, g, b): (u8, u8, u8)) -> &'static str {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 0x28 {
        return match max {
            0..0x40 => "black",
            0x40..0xA0 => "darkGray",
            0xA0..0xE0 => "lightGray",
            _ => "white",
        };
    }
    let (rf, gf, bf) = (f32::from(r), f32::from(g), f32::from(b));
    let chroma = f32::from(max - min);
    let hue = if max == r {
        60.0 * ((gf - bf) / chroma).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((bf - rf) / chroma + 2.0)
    } else {
        60.0 * ((rf - gf) / chroma + 4.0)
    };
    let bright = max >= 0xC0;
    let names = [
        ("red", "darkRed"),
        ("yellow", "darkYellow"),
        ("green", "darkGreen"),
        ("cyan", "darkCyan"),
        ("blue", "darkBlue"),
        ("magenta", "darkMagenta"),
    ];
    let (light, dark) = names[((hue / 60.0).round() as usize) % 6];
    if bright { light } else { dark }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_highlight_is_its_own_nearest() {
        for &(name, rgb) in HIGHLIGHTS {
            assert_eq!(nearest_highlight(rgb), name);
            assert_eq!(highlight_rgb(name), Some(rgb));
        }
        assert_eq!(highlight_rgb("none"), None);
    }

    #[test]
    fn a_pale_or_off_colour_takes_its_hue() {
        assert_eq!(nearest_highlight((0xFF, 0xFC, 0x9E)), "yellow");
        assert_eq!(nearest_highlight((0xFF, 0xD0, 0xD0)), "red");
        assert_eq!(nearest_highlight((0x20, 0x60, 0x20)), "darkGreen");
        assert_eq!(nearest_highlight((0xFE, 0xFE, 0xFE)), "white");
        assert_eq!(nearest_highlight((0xF0, 0x40, 0xF8)), "magenta");
    }
}
