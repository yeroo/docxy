//! Word's dark mode for the document view (#2079): the colours a document
//! draws in on a dark page, computed from its own colours at draw time.
//!
//! Display only. The document model, saving, printing and PDF export keep
//! the document's real colours; only the view's palette (`Pal` in main.rs)
//! maps them. Every function here is pure `0xRRGGBB` arithmetic, so the rules
//! are unit-tested without a window:
//!
//! - the page is [`PAGE_DARK`] on a darker canvas ([`CANVAS_DARK`]);
//! - a light fill ([`fill`], HSL lightness at least [`LIGHT_FILL`]) is
//!   darkened keeping its hue, monotone in lightness (white becomes the page,
//!   a darker light fill a lighter dark one, so banded tables keep their
//!   bands); every other fill is kept;
//! - text ([`text`]) is kept when it already reads (4.5:1, WCAG AA) on the
//!   mapped colour under it, so text the author chose against a kept fill
//!   stays; otherwise a dark colour on a dark ground has its lightness
//!   inverted, and then its lightness moves away from the ground, hue and
//!   saturation kept, until it reads;
//! - borders ([`border`]) take the same rule with the 3:1 floor of non-text
//!   contrast; an automatic border is a mid grey ([`BORDER_DARK`]).

/// The page, as Word's dark mode draws it.
pub(crate) const PAGE_DARK: u32 = 0x262626;
/// Around the page in Print Layout: darker than the page so its edge shows.
pub(crate) const CANVAS_DARK: u32 = 0x141414;
/// Automatic text on a dark ground.
pub(crate) const AUTO_DARK: u32 = 0xF2F2F2;
/// Automatic text on a light ground (as `design_tab::page_ink` draws it).
const AUTO_LIGHT: u32 = 0x202020;
/// An automatic border or rule on a dark ground: 4.5:1 on [`PAGE_DARK`].
pub(crate) const BORDER_DARK: u32 = 0x8C8C8C;
/// Dimmed text (the light grey `design_tab::page_ink` dims with on a dark
/// page), floor-checked like any text.
const DIM_DARK: u32 = 0xB0B0B0;
const DIM_LIGHT: u32 = 0x6E6E6E;
/// An uncoloured link on a light ground (the suite's `LINK`).
const LINK_LIGHT: u32 = 0x2F6FDB;
/// An uncoloured link on a dark ground (the suite's `LINK_DARK`).
const LINK_DARK: u32 = 0x6CA0F0;
/// The contrast every run keeps against what it sits on (WCAG AA).
pub(crate) const TEXT_FLOOR: f32 = 4.5;
/// The contrast a border keeps (WCAG non-text contrast).
pub(crate) const BORDER_FLOOR: f32 = 3.0;
/// The HSL lightness from which a fill counts as light and is darkened.
pub(crate) const LIGHT_FILL: f32 = 0.8;
/// How far a light fill's distance from white carries into the dark: a fill
/// at the [`LIGHT_FILL`] threshold lands this far above the page.
const FILL_SPREAD: f32 = 1.0;
/// The extra lightness a tinted light fill gets, so a pastel callout reads
/// as its own area rather than the page.
const TINT_LIFT: f32 = 0.06;
/// The saturation from which a light fill counts as tinted.
const TINT_MIN_SAT: f32 = 0.15;
/// The saturation a darkened tint keeps at most, so it reads blue-grey (not
/// a vivid dark blue).
const TINT_MAX_SAT: f32 = 0.35;

fn channels(rgb: u32) -> [f32; 3] {
    [16, 8, 0].map(|s| f32::from(((rgb >> s) & 0xFF) as u8) / 255.0)
}

fn pack([r, g, b]: [f32; 3]) -> u32 {
    let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}

/// WCAG relative luminance of `rgb`.
pub(crate) fn luminance(rgb: u32) -> f32 {
    let lin = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = channels(rgb).map(lin);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG contrast ratio between `a` and `b` (1 to 21).
pub(crate) fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Whether text on `ground` reads better light than dark: white has more
/// contrast on it than black. Either way the better one reaches at least
/// 4.58:1, so moving toward it always meets [`TEXT_FLOOR`].
pub(crate) fn takes_light(ground: u32) -> bool {
    contrast(0xFFFFFF, ground) >= contrast(0x000000, ground)
}

/// `rgb` as (hue in degrees, saturation, lightness).
pub(crate) fn hsl(rgb: u32) -> (f32, f32, f32) {
    let [r, g, b] = channels(rgb);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s.min(1.0), l)
}

/// The colour of hue `h` (degrees), saturation `s` and lightness `l`.
pub(crate) fn from_hsl(h: f32, s: f32, l: f32) -> u32 {
    let l = l.clamp(0.0, 1.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s.clamp(0.0, 1.0);
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let [r, g, b] = match hp as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    let m = l - c / 2.0;
    pack([r + m, g + m, b + m])
}

/// The colour a fill of `rgb` draws in on a dark page. A light one (HSL
/// lightness at least [`LIGHT_FILL`]) is darkened, keeping its hue,
/// monotone in lightness: white becomes [`PAGE_DARK`], and a darker light
/// fill a lighter dark one, so `#F2F2F2` and `#D9D9D9` bands keep their
/// order. A tinted one also gets [`TINT_LIFT`] and at most [`TINT_MAX_SAT`]
/// saturation, so a pastel callout reads as a dark blue-grey area of its
/// own. Every other fill (vivid, mid, dark) is kept, and the text on it is
/// chosen against it ([`text`]). The threshold is a step: a fill just under
/// it is kept as it is.
pub(crate) fn fill(rgb: u32) -> u32 {
    let (h, s, l) = hsl(rgb);
    if l < LIGHT_FILL {
        return rgb;
    }
    let page_l = hsl(PAGE_DARK).2;
    let tinted = s >= TINT_MIN_SAT;
    let dark_l = page_l + (1.0 - l) * FILL_SPREAD + if tinted { TINT_LIFT } else { 0.0 };
    if tinted {
        from_hsl(h, s.min(TINT_MAX_SAT), dark_l)
    } else {
        // A near-grey stays a grey: a faint cast would only read as dirt.
        pack([dark_l; 3])
    }
}

/// The colour a Print Layout sheet draws in on a dark page: [`PAGE_DARK`],
/// or the document's page colour mapped as a fill (a gold page stays gold, a
/// pastel one is darkened).
pub(crate) fn page_sheet(page_colour: Option<u32>) -> u32 {
    page_colour.map_or(PAGE_DARK, fill)
}

/// The least lightness at or above `from` (hue `h`, saturation `s`) whose
/// colour has at least `min` contrast against `ground`, lighter than it.
fn lighten(h: f32, s: f32, from: f32, ground: u32, min: f32) -> Option<u32> {
    let y = luminance(ground);
    let reads = |l: f32| {
        let c = from_hsl(h, s, l);
        let yc = luminance(c);
        (yc >= y && (yc + 0.05) / (y + 0.05) >= min).then_some(c)
    };
    reads(1.0)?;
    if let Some(c) = reads(from) {
        return Some(c);
    }
    // Lightness raises every channel, so the luminance (and, above the
    // ground, the contrast) grows with it: bisect for the least that reads.
    let (mut lo, mut hi) = (from, 1.0);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if reads(mid).is_some() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    reads(hi)
}

/// The greatest lightness at or below `from` whose colour has at least
/// `min` contrast against `ground`, darker than it.
fn darken(h: f32, s: f32, from: f32, ground: u32, min: f32) -> Option<u32> {
    let y = luminance(ground);
    let reads = |l: f32| {
        let c = from_hsl(h, s, l);
        let yc = luminance(c);
        (yc <= y && (y + 0.05) / (yc + 0.05) >= min).then_some(c)
    };
    reads(0.0)?;
    if let Some(c) = reads(from) {
        return Some(c);
    }
    let (mut lo, mut hi) = (0.0, from);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if reads(mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    reads(lo)
}

/// `rgb` moved until it has `min` contrast on `ground`: kept when it does
/// already; on a ground that takes light ink, a dark colour (HSL lightness
/// under 0.5) has its lightness inverted, capped at [`AUTO_DARK`]'s, and is
/// lightened from there; on one that takes dark ink it is darkened. Hue and
/// saturation are kept. Should that side not reach `min` (a mid ground), the
/// other side is taken, and at worst white or black, which always reach
/// 4.58:1.
fn floored(rgb: u32, ground: u32, min: f32) -> u32 {
    if contrast(rgb, ground) >= min {
        return rgb;
    }
    let (h, s, l) = hsl(rgb);
    let light = takes_light(ground);
    let up = || {
        let cap = hsl(AUTO_DARK).2;
        let from = if l < 0.5 {
            (1.0 - l).min(cap).max(l)
        } else {
            l
        };
        lighten(h, s, from, ground, min)
    };
    let down = || darken(h, s, l, ground, min);
    let moved = if light {
        up().or_else(down)
    } else {
        down().or_else(up)
    };
    moved.unwrap_or(if light { 0xFFFFFF } else { 0x000000 })
}

/// `over` laid at opacity `a` on `base`, as one opaque colour.
pub(crate) fn blend(base: u32, over: u32, a: f32) -> u32 {
    let (b, o) = (channels(base), channels(over));
    pack([0, 1, 2].map(|i| b[i] + (o[i] - b[i]) * a))
}

/// The inks and the chip wash of a palette on `ground`: automatic text,
/// dimmed text and an uncoloured link, and the wash its chips and hover
/// draw in (a field's shading, a picture's stand-in), as (colour, opacity).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Inks {
    pub fg: u32,
    pub dim: u32,
    pub link: u32,
    pub wash: (u32, f32),
}

/// `rgb` moved, on the side `light` (lighter than the ground, else darker),
/// to `min` contrast against `ground`, hue kept: as [`floored`] does, but
/// never to the other side; `None` where that side cannot reach it.
fn on_side(rgb: u32, ground: u32, min: f32, light: bool) -> Option<u32> {
    let (y, yg) = (luminance(rgb), luminance(ground));
    if (if light { y >= yg } else { y <= yg }) && contrast(rgb, ground) >= min {
        return Some(rgb);
    }
    let (h, s, l) = hsl(rgb);
    if light {
        let from = if l < 0.5 {
            (1.0 - l).min(hsl(AUTO_DARK).2).max(l)
        } else {
            l
        };
        lighten(h, s, from, ground, min)
    } else {
        darken(h, s, l, ground, min)
    }
}

/// The [`Inks`] of a palette on `ground`. The inks' side is the ground's
/// ([`takes_light`]), as [`text`] maps a run's colour. The wash is the one
/// that shows (light on a ground that takes light ink, dark on one that
/// takes dark), which moves the chip toward the ink: the inks are chosen
/// against the chip, the harder ground, so they read on it and on the
/// plain ground. Near the crossover no ink on that side may read on such a
/// chip (`#4472C4`: white is 4.73:1 on it but 3.93:1 on its light chip);
/// then the wash goes away from the ink, which only raises the contrast,
/// and the inks are chosen against the ground, where that side always
/// reaches the floor.
pub(crate) fn inks(ground: u32) -> Inks {
    let light = takes_light(ground);
    let bases = if light {
        [AUTO_DARK, DIM_DARK, LINK_DARK]
    } else {
        [AUTO_LIGHT, DIM_LIGHT, LINK_LIGHT]
    };
    let toward = if light {
        (0xFFFFFF, 0.10)
    } else {
        (0x000000, 0.06)
    };
    let chip = blend(ground, toward.0, toward.1);
    let picked = bases.map(|b| {
        on_side(b, chip, TEXT_FLOOR, light).filter(|&i| contrast(i, ground) >= TEXT_FLOOR)
    });
    if let [Some(fg), Some(dim), Some(link)] = picked {
        return Inks {
            fg,
            dim,
            link,
            wash: toward,
        };
    }
    let away = if light {
        (0x000000, 0.10)
    } else {
        (0xFFFFFF, 0.10)
    };
    let [fg, dim, link] = bases.map(|b| {
        on_side(b, ground, TEXT_FLOOR, light).unwrap_or(if light { 0xFFFFFF } else { 0x000000 })
    });
    Inks {
        fg,
        dim,
        link,
        wash: away,
    }
}

/// `ground` under its palette's wash ([`inks`]): where a chip's text sits.
pub(crate) fn chip(ground: u32) -> u32 {
    let (c, a) = inks(ground).wash;
    blend(ground, c, a)
}

/// The ground of an open header or footer on a sheet of colour `sheet`:
/// opaque, so the area shows as being edited, and between the sheet and
/// its ink, so ink chosen against the tint also reads on the plain sheet
/// (a frame of the part placed off its band sits there): a faint
/// light-blue wash on a sheet that takes light ink, a faint darkening on
/// one that takes dark ink. A wash that would move the tint to the other
/// side of the crossover is not drawn: the sheet itself. (Print Layout
/// tints a light page with `#EEF4FF` at half opacity instead.)
pub(crate) fn edit_tint(sheet: u32) -> u32 {
    let light = takes_light(sheet);
    let tint = if light {
        blend(sheet, 0xEEF4FF, 0.10)
    } else {
        blend(sheet, 0x000000, 0.06)
    };
    if takes_light(tint) == light {
        tint
    } else {
        sheet
    }
}

/// The colour text of colour `rgb` draws in on the mapped ground `ground`
/// (the page, or a cell's [`fill`]): at least [`TEXT_FLOOR`] against it,
/// hue kept. `#000000` and `#595959` on the page invert to `#F2F2F2` and
/// about `#A6A6A6`; `#0047FF` (lightness 0.5) is lightened to a lighter
/// blue; dark text on a kept `#5B9BD5` fill already reads and is kept.
pub(crate) fn text(rgb: u32, ground: u32) -> u32 {
    floored(rgb, ground, TEXT_FLOOR)
}

/// Automatic text on `ground`: [`AUTO_DARK`] where light ink reads better,
/// else a near-black, floor-checked.
pub(crate) fn auto_text(ground: u32) -> u32 {
    text(
        if takes_light(ground) {
            AUTO_DARK
        } else {
            AUTO_LIGHT
        },
        ground,
    )
}

/// Dimmed text on `ground` (the palette's `dim`), floor-checked.
pub(crate) fn dim_text(ground: u32) -> u32 {
    text(
        if takes_light(ground) {
            DIM_DARK
        } else {
            DIM_LIGHT
        },
        ground,
    )
}

/// A border or rule of colour `rgb` (`None`: automatic) on `ground`: an
/// automatic one is [`BORDER_DARK`] (or a dark grey where dark ink reads),
/// an explicit one kept at [`BORDER_FLOOR`] or moved as [`text`] moves
/// text, to that floor.
pub(crate) fn border(rgb: Option<u32>, ground: u32) -> u32 {
    let c = rgb.unwrap_or(if takes_light(ground) {
        BORDER_DARK
    } else {
        0x595959
    });
    floored(c, ground, BORDER_FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hue_gap(a: f32, b: f32) -> f32 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    #[test]
    fn contrast_matches_wcag() {
        assert!((contrast(0x000000, 0xFFFFFF) - 21.0).abs() < 0.01);
        assert!((contrast(0x777777, 0x777777) - 1.0).abs() < 0.001);
        // Word's Accent 1 with black: just under the floor.
        let c = contrast(0x000000, 0x4472C4);
        assert!((4.40..4.5).contains(&c), "{c}");
    }

    #[test]
    fn hsl_round_trips() {
        for rgb in [
            0x000000, 0xFFFFFF, 0x4472C4, 0x0047FF, 0xEEF3FF, 0x595959, 0xFFC000,
        ] {
            let (h, s, l) = hsl(rgb);
            assert_eq!(from_hsl(h, s, l), rgb, "{rgb:06x}");
        }
    }

    #[test]
    fn page_and_canvas() {
        assert!(luminance(CANVAS_DARK) < luminance(PAGE_DARK));
        assert!(contrast(BORDER_DARK, PAGE_DARK) >= BORDER_FLOOR);
        assert_eq!(page_sheet(None), PAGE_DARK);
    }

    #[test]
    fn automatic_text_is_light_on_the_page() {
        assert_eq!(auto_text(PAGE_DARK), AUTO_DARK);
        assert!(contrast(auto_text(PAGE_DARK), PAGE_DARK) >= TEXT_FLOOR);
        // On a light ground (a host's light page) it is dark.
        assert_eq!(auto_text(0xFFFFFF), AUTO_LIGHT);
    }

    #[test]
    fn black_text_inverts_to_the_automatic_light() {
        assert_eq!(text(0x000000, PAGE_DARK), AUTO_DARK);
    }

    #[test]
    fn dark_greys_lighten_keeping_grey() {
        let g = text(0x595959, PAGE_DARK);
        let (_, s, l) = hsl(g);
        assert!(s < 0.01, "{g:06x}");
        assert!((0.62..0.7).contains(&l), "{g:06x} lightness {l}");
        assert!(contrast(g, PAGE_DARK) >= TEXT_FLOOR);
        let g = text(0x7F7F7F, PAGE_DARK);
        assert!(hsl(g).2 > hsl(0x7F7F7F).2, "{g:06x}");
        assert!(contrast(g, PAGE_DARK) >= TEXT_FLOOR);
    }

    #[test]
    fn a_deep_accent_blue_lightens_keeping_its_hue() {
        let b = text(0x0047FF, PAGE_DARK);
        assert_ne!(b, 0x0047FF);
        let (h, s, l) = hsl(b);
        assert!(hue_gap(h, hsl(0x0047FF).0) < 3.0, "{b:06x}");
        assert!(s > 0.9, "{b:06x}");
        assert!(l > 0.5, "{b:06x}");
        assert!(contrast(b, PAGE_DARK) >= TEXT_FLOOR);
        // Not washed out to white: the least lightening that reads.
        assert!(l < 0.8, "{b:06x}");
    }

    #[test]
    fn text_that_reads_is_kept() {
        assert_eq!(text(0xF2F2F2, PAGE_DARK), 0xF2F2F2);
        assert_eq!(text(0x9CC3FF, PAGE_DARK), 0x9CC3FF);
    }

    #[test]
    fn a_saturated_fill_is_kept_and_dark_text_on_it_stays_dark() {
        assert_eq!(fill(0x5B9BD5), 0x5B9BD5);
        assert!(contrast(0x000000, 0x5B9BD5) > 6.5);
        assert_eq!(text(0x000000, 0x5B9BD5), 0x000000);
        assert_eq!(text(0x1F1F1F, 0x5B9BD5), 0x1F1F1F);
        assert!(!takes_light(0x5B9BD5));
        // Automatic text on it is dark too.
        assert!(luminance(auto_text(0x5B9BD5)) < 0.05);
    }

    #[test]
    fn word_accent_one_keeps_its_fill_and_turns_black_text_light() {
        // Black reaches only 4.45:1 on #4472C4, white 4.73: the floor wants
        // the light side.
        assert_eq!(fill(0x4472C4), 0x4472C4);
        let t = text(0x000000, 0x4472C4);
        assert!(luminance(t) > 0.8, "{t:06x}");
        assert!(contrast(t, 0x4472C4) >= TEXT_FLOOR);
    }

    #[test]
    fn vivid_and_dark_fills_are_kept() {
        for f in [0xFFC000, 0x0047FF, 0x4472C4, 0x1F3864, 0xC00000, 0xFFFF00] {
            assert_eq!(fill(f), f, "{f:06x}");
        }
    }

    #[test]
    fn a_very_light_tint_darkens_keeping_its_hue() {
        let f = fill(0xEEF3FF);
        let (h, s, l) = hsl(f);
        assert!(hue_gap(h, hsl(0xEEF3FF).0) < 4.0, "{f:06x}");
        assert!((0.18..=0.3).contains(&l), "{f:06x} lightness {l}");
        assert!(s > 0.1 && s <= TINT_MAX_SAT + 0.01, "{f:06x}");
        // Its own area: distinguishable from the page.
        assert!(contrast(f, PAGE_DARK) > 1.15, "{f:06x}");
        // Automatic text on it is light and reads.
        let t = auto_text(f);
        assert!(luminance(t) > 0.8, "{t:06x}");
        assert!(contrast(t, f) >= TEXT_FLOOR);
        // So does black text and the dim grey.
        assert!(contrast(text(0x000000, f), f) >= TEXT_FLOOR);
        assert!(contrast(dim_text(f), f) >= TEXT_FLOOR);
    }

    #[test]
    fn white_fill_becomes_the_page() {
        assert_eq!(fill(0xFFFFFF), PAGE_DARK);
    }

    #[test]
    fn light_grey_fills_keep_their_order() {
        let (a, b) = (fill(0xF2F2F2), fill(0xD9D9D9));
        assert_ne!(a, b);
        assert!(luminance(a) > luminance(PAGE_DARK), "{a:06x}");
        assert!(luminance(b) > luminance(a), "{b:06x} vs {a:06x}");
        // Monotone over the whole light range.
        let mut prev = luminance(fill(0xFFFFFF));
        for v in (0xCC..0xFF).rev() {
            let y = luminance(fill(v * 0x010101));
            assert!(y >= prev, "{v:02x}");
            prev = y;
        }
    }

    #[test]
    fn page_sheet_keeps_a_gold_page_and_darkens_a_pastel_one() {
        assert_eq!(page_sheet(Some(0xFFC000)), 0xFFC000);
        assert!(!takes_light(0xFFC000));
        let p = page_sheet(Some(0xFDE9D9));
        assert!(hsl(p).2 < 0.35, "{p:06x}");
    }

    #[test]
    fn borders_stay_visible() {
        assert_eq!(border(None, PAGE_DARK), BORDER_DARK);
        assert!(contrast(border(None, PAGE_DARK), PAGE_DARK) >= BORDER_FLOOR);
        // A dark explicit border is lightened to at least 3:1, hue kept.
        let b = border(Some(0x1F3864), PAGE_DARK);
        assert!(contrast(b, PAGE_DARK) >= BORDER_FLOOR, "{b:06x}");
        assert!(hue_gap(hsl(b).0, hsl(0x1F3864).0) < 4.0, "{b:06x}");
        // A visible one is kept.
        assert_eq!(border(Some(0xBFBFBF), PAGE_DARK), 0xBFBFBF);
        // On a kept light fill the automatic border is dark and visible.
        assert!(contrast(border(None, 0xFFC000), 0xFFC000) >= BORDER_FLOOR);
    }

    #[test]
    fn an_open_headers_tint_shows_and_keeps_its_text_readable() {
        let t = edit_tint(PAGE_DARK);
        // Lighter than the sheet, visibly, and still a dark ground.
        assert!(luminance(t) > luminance(PAGE_DARK));
        assert!(contrast(t, PAGE_DARK) > 1.2, "{t:06x}");
        assert!(takes_light(t));
        // Text chosen against the tint reads on it, and on the plain sheet
        // the same live header draws on other pages.
        for c in [0x000000, 0x595959, 0x0047FF, 0x0563C1, AUTO_DARK, 0xC00000] {
            let ink = text(c, t);
            assert!(contrast(ink, t) >= TEXT_FLOOR, "{c:06x} -> {ink:06x}");
            assert!(
                contrast(ink, PAGE_DARK) >= TEXT_FLOOR,
                "{c:06x} -> {ink:06x}"
            );
        }
    }

    #[test]
    fn the_chip_shows_where_it_can_and_moves_away_from_the_ink_where_not() {
        assert_eq!(blend(0x000000, 0xFFFFFF, 0.5), 0x808080);
        // Dark ink on a red fill: a faint dark wash that shows.
        assert!(!takes_light(0xFF0000));
        assert!(luminance(chip(0xFF0000)) < luminance(0xFF0000));
        // On the page: a light wash that shows.
        assert!(luminance(chip(PAGE_DARK)) > luminance(PAGE_DARK));
        // Word's Accent 1 takes light ink, which a light chip would leave
        // under the floor (white 3.93:1 on #5780CA): a dark chip instead.
        assert!(takes_light(0x4472C4));
        assert!(luminance(chip(0x4472C4)) < luminance(0x4472C4));
    }

    #[test]
    fn automatic_text_and_black_take_the_same_side_on_accent_one() {
        let i = inks(0x4472C4);
        let black = text(0x000000, 0x4472C4);
        assert!(luminance(i.fg) > luminance(0x4472C4), "{:06x}", i.fg);
        assert!(luminance(black) > luminance(0x4472C4), "{black:06x}");
        assert!(contrast(i.fg, 0x4472C4) >= TEXT_FLOOR);
    }

    #[test]
    fn a_palettes_inks_read_on_its_ground_and_its_chip_on_the_grounds_side() {
        let grounds = [
            PAGE_DARK, 0x4472C4, 0x0070C0, 0x2E74B5, 0x6E6E6E, 0x767676, 0x7C7C7C, 0x5B9BD5,
            0xFF0000, 0xFFC000, 0x808080, 0x0047FF, 0x1F3864, 0x70AD47, 0x00B0F0, 0x0A0A0A,
            0xFFFFFF, 0x262626, 0x3A3B3C, 0xC00000, 0x7030A0, 0x00B050,
        ];
        // And a grey ramp across the crossover.
        let ramp = (0x50..=0x90).step_by(2).map(|v: u32| v * 0x010101);
        for g in grounds.into_iter().chain(ramp).chain(grounds.map(fill)) {
            let i = inks(g);
            let c = blend(g, i.wash.0, i.wash.1);
            for (name, ink) in [("fg", i.fg), ("dim", i.dim), ("link", i.link)] {
                for on in [g, c] {
                    let r = contrast(ink, on);
                    assert!(
                        r >= TEXT_FLOOR,
                        "{name} {ink:06x} on {on:06x} (ground {g:06x}): {r:.2}"
                    );
                }
                assert_eq!(
                    luminance(ink) > luminance(g),
                    takes_light(g),
                    "{name} {ink:06x} on {g:06x} is on the other side"
                );
            }
        }
    }

    #[test]
    fn an_open_headers_tint_on_a_gold_page_keeps_its_runs_readable_on_the_sheet() {
        let sheet = page_sheet(Some(0xFFC000));
        let t = edit_tint(sheet);
        assert_ne!(t, sheet);
        assert!(luminance(t) < luminance(sheet));
        for c in [0xC00000, 0x6E6E6E, 0x000000, 0x0047FF] {
            let ink = text(c, t);
            for on in [t, sheet] {
                assert!(
                    contrast(ink, on) >= TEXT_FLOOR,
                    "{c:06x} -> {ink:06x} on {on:06x}"
                );
            }
        }
        // A sheet at the crossover keeps no tint that would flip its side.
        for v in (0x60..=0x80).map(|v: u32| v * 0x010101) {
            assert_eq!(takes_light(edit_tint(v)), takes_light(v), "{v:06x}");
        }
    }

    #[test]
    fn every_mapped_text_reads_on_every_mapped_ground() {
        let fills = [
            0xFFFFFF, 0xEEF3FF, 0xF2F2F2, 0xD9D9D9, 0xDEEAF6, 0xFFF2CC, 0xE2EFD9, 0x5B9BD5,
            0x4472C4, 0xFFC000, 0x0047FF, 0x1F3864, 0x808080, 0x7F7F7F, 0xC00000, 0x70AD47,
            0xFFFF00, 0x00B0F0, 0x262626, 0x000000, 0xCCCCCC, 0xB4C6E7,
        ];
        let mut grounds = vec![PAGE_DARK];
        grounds.extend(fills.iter().map(|&f| fill(f)));
        for &g in &grounds {
            for r in (0..=0xFF).step_by(0x33) {
                for gr in (0..=0xFF).step_by(0x33) {
                    for b in (0..=0xFF).step_by(0x33) {
                        let c = (r << 16) | (gr << 8) | b;
                        let t = text(c, g);
                        assert!(
                            contrast(t, g) >= TEXT_FLOOR,
                            "{c:06x} on {g:06x} -> {t:06x}: {}",
                            contrast(t, g)
                        );
                    }
                }
            }
            assert!(contrast(auto_text(g), g) >= TEXT_FLOOR, "auto on {g:06x}");
            assert!(contrast(dim_text(g), g) >= TEXT_FLOOR, "dim on {g:06x}");
            assert!(
                contrast(border(None, g), g) >= BORDER_FLOOR,
                "border on {g:06x}"
            );
        }
    }
}
