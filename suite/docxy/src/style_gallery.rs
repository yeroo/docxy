//! The Home ribbon's Styles gallery tiles: their geometry, each style's
//! "AaBbCc" look per theme, and the tile-state colours.
//!
//! This is the one table both renderers draw from. The suite's
//! `style_gallery()` reads it directly; `ribbon_export` writes it into
//! `ribbon-docx.json`, which the editable-HTML page (`htmlbundle/web`) turns
//! into its gallery stylesheet. A change here without the snapshot, or to the
//! snapshot without this, fails the `ribbon_export` drift test.
//!
//! Colours are plain sRGB [`Rgba`] so the arithmetic is testable without a
//! window: the tile states are the theme's own tokens mixed in sRGB, exactly
//! what CSS `color-mix(in srgb, …)` computes.

use gpui::Rgba;

/// A tile's and the gallery well's geometry, in logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileGeom {
    /// The tile's outer size, border included.
    pub w: f32,
    pub h: f32,
    /// Padding inside the tile's border.
    pub pad: f32,
    pub radius: f32,
    /// Every tile has this border, transparent unless selected, so selecting
    /// one never moves its text.
    pub border: f32,
    /// Space between tiles.
    pub gap: f32,
    /// The well: the bordered box the tiles sit in.
    pub well_pad: f32,
    pub well_radius: f32,
    pub well_border: f32,
    /// The style name's font size.
    pub name_size: f32,
    /// The preview text.
    pub sample: &'static str,
}

pub const TILE: TileGeom = TileGeom {
    w: 64.,
    h: 62.,
    pad: 4.,
    radius: 3.,
    border: 1.,
    gap: 2.,
    well_pad: 2.,
    well_radius: 4.,
    well_border: 1.,
    name_size: 10.,
    sample: "AaBbCc",
};

/// The well's outer width for `n` tiles.
pub fn well_width(n: usize) -> f32 {
    let n = n as f32;
    n * TILE.w + (n - 1.).max(0.) * TILE.gap + 2. * (TILE.well_pad + TILE.well_border)
}

/// The well's outer height.
pub fn well_height() -> f32 {
    TILE.h + 2. * (TILE.well_pad + TILE.well_border)
}

/// How the tile states are mixed from the theme, in percent of the second
/// colour (CSS `color-mix(in srgb, <second> <pct>%, <first>)`).
///
/// - surface: the theme background toward the foreground. Per theme, because
///   dark needs more to sit just above the ribbon's own fill (`secondary`).
/// - hover: the surface toward the foreground.
/// - checked: the surface toward the brand accent.
pub fn surface_mix(dark: bool) -> f32 {
    if dark { 16. } else { 6. }
}
pub const HOVER_MIX: f32 = 12.;
pub const CHECKED_MIX: f32 = 20.;

/// A sample's colour: the theme foreground, or a fixed colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// The theme foreground.
    Fg,
    Rgb(u32),
}

impl Ink {
    /// The name the snapshot uses: `fg` or `#rrggbb`.
    #[cfg(test)]
    pub fn css(self) -> String {
        match self {
            Ink::Fg => "fg".into(),
            Ink::Rgb(c) => format!("#{c:06x}"),
        }
    }
}

/// How one style's "AaBbCc" is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SampleLook {
    pub size: f32,
    /// CSS weight. Word's Title and Headings are Calibri Light, which is
    /// family Calibri at 300 to DirectWrite and CSS alike.
    pub weight: u16,
    pub ink: Ink,
}

/// The preview ids the gallery uses (`ribbonspec::GalleryItem::preview`).
#[cfg(test)]
pub const PREVIEWS: [&str; 6] = ["normal", "h1", "h2", "h3", "title", "subtitle"];

/// The document font the samples are drawn in.
pub const SAMPLE_FAMILY: &str = "Calibri";

/// A style's look in one theme. An unknown preview id draws as body text.
pub fn sample_look(preview: &str, dark: bool) -> SampleLook {
    // Word 2016's defaults: headings in the theme's accent blue (lighter in
    // dark so they read on the dark tile), Subtitle grey, Title and body text
    // in the foreground.
    let (size, weight, light, dark_ink) = match preview {
        "h1" => (16., 300, Ink::Rgb(0x2F5496), Ink::Rgb(0x9EC1F7)),
        "h2" => (14., 300, Ink::Rgb(0x2F5496), Ink::Rgb(0x9EC1F7)),
        "h3" => (13., 300, Ink::Rgb(0x1F3763), Ink::Rgb(0x9EC1F7)),
        "title" => (18., 300, Ink::Fg, Ink::Fg),
        "subtitle" => (12., 400, Ink::Rgb(0x5A5A5A), Ink::Rgb(0xBCBCBC)),
        _ => (13., 400, Ink::Fg, Ink::Fg),
    };
    SampleLook {
        size,
        weight,
        ink: if dark { dark_ink } else { light },
    }
}

/// The colours a tile is drawn with, from the theme's tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileColors {
    pub well: Rgba,
    pub surface: Rgba,
    pub hover: Rgba,
    pub checked: Rgba,
}

pub fn tile_colors(background: Rgba, foreground: Rgba, brand: Rgba, dark: bool) -> TileColors {
    let surface = mix(background, foreground, surface_mix(dark));
    TileColors {
        well: background,
        surface,
        hover: mix(surface, foreground, HOVER_MIX),
        checked: mix(surface, brand, CHECKED_MIX),
    }
}

/// `a` moved `pct` percent toward `b`, in sRGB.
pub fn mix(a: Rgba, b: Rgba, pct: f32) -> Rgba {
    let t = pct / 100.;
    let m = |x: f32, y: f32| x + (y - x) * t;
    Rgba {
        r: m(a.r, b.r),
        g: m(a.g, b.g),
        b: m(a.b, b.b),
        a: m(a.a, b.a),
    }
}

/// An ink's colour in a theme.
pub fn ink_rgba(ink: Ink, foreground: Rgba) -> Rgba {
    match ink {
        Ink::Fg => foreground,
        Ink::Rgb(c) => gpui::rgb(c),
    }
}

/// WCAG 2 contrast ratio of two opaque colours.
#[cfg(test)]
pub fn contrast(a: Rgba, b: Rgba) -> f32 {
    fn lum(c: Rgba) -> f32 {
        let ch = |v: f32| {
            // Rounded to 8 bits first: that is what reaches the screen.
            let v = (v.clamp(0., 1.) * 255.).round() / 255.;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
    }
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::theme::ThemeColor;

    fn theme(dark: bool) -> ThemeColor {
        if dark {
            *ThemeColor::dark()
        } else {
            *ThemeColor::light()
        }
    }

    #[test]
    fn every_gallery_preview_has_its_own_look() {
        let ribbon = crate::docxy_ribbon();
        let mut seen = Vec::new();
        for tab in &ribbon.tabs {
            for g in &tab.groups {
                for c in &g.items {
                    if let crate::Control::Gallery(gal) = c {
                        seen.extend(gal.items.iter().map(|i| i.preview));
                    }
                }
            }
        }
        assert!(!seen.is_empty(), "the docx ribbon has a Styles gallery");
        for p in seen {
            assert!(
                PREVIEWS.contains(&p),
                "preview '{p}' has no row in the table"
            );
        }
        // No two rows collapse into the fallback: each named id is distinct
        // from body text except `normal` itself.
        for p in PREVIEWS.iter().filter(|p| **p != "normal") {
            assert_ne!(sample_look(p, false), sample_look("normal", false), "{p}");
        }
    }

    /// Every sample must read on every state of its tile, in both themes, at
    /// 4.5:1: none of them is WCAG "large" text (24px, or 18.66px bold). The
    /// name is `pal.dim`, the suite's muted chrome text everywhere, held to
    /// 3:1 like the rest of that chrome.
    #[test]
    fn samples_and_names_have_enough_contrast_on_every_tile_state() {
        let brand = gpui::rgb(crate::BRAND);
        for dark in [false, true] {
            let t = theme(dark);
            let (fg, muted) = (t.foreground.to_rgb(), t.muted_foreground.to_rgb());
            let c = tile_colors(t.background.to_rgb(), fg, brand, dark);
            for (state, fill) in [
                ("surface", c.surface),
                ("hover", c.hover),
                ("checked", c.checked),
            ] {
                for p in PREVIEWS {
                    let ink = ink_rgba(sample_look(p, dark).ink, fg);
                    let r = contrast(ink, fill);
                    assert!(r >= 4.5, "{p} on {state} (dark={dark}): {r:.2}");
                }
                let r = contrast(muted, fill);
                assert!(r >= 3.0, "name on {state} (dark={dark}): {r:.2}");
            }
        }
    }

    /// The owner's words: a light grey tile on white; in dark, a tile a little
    /// lighter than the ribbon around it. Hover and selected must differ from
    /// the plain tile and from each other.
    #[test]
    fn the_tile_surface_is_where_the_owner_asked_for_it() {
        let lum = |c: Rgba| c.r + c.g + c.b;
        let brand = gpui::rgb(crate::BRAND);
        let light = theme(false);
        let c = tile_colors(
            light.background.to_rgb(),
            light.foreground.to_rgb(),
            brand,
            false,
        );
        assert!(lum(c.surface) < lum(c.well), "light: grey on white");
        let dark = theme(true);
        let c = tile_colors(
            dark.background.to_rgb(),
            dark.foreground.to_rgb(),
            brand,
            true,
        );
        assert!(
            lum(c.surface) > lum(dark.secondary.to_rgb()),
            "dark: lighter than the ribbon's fill"
        );
        for dark in [false, true] {
            let t = theme(dark);
            let c = tile_colors(t.background.to_rgb(), t.foreground.to_rgb(), brand, dark);
            assert_ne!(c.hover, c.surface);
            assert_ne!(c.checked, c.surface);
            assert_ne!(c.checked, c.hover);
        }
    }

    #[test]
    fn the_well_is_the_tiles_plus_its_padding_and_border() {
        assert_eq!(well_width(7), 466.);
        assert_eq!(well_width(1), 70.);
        assert_eq!(well_height(), 68.);
    }

    /// The ribbon is 98px; its group has 2px padding top and bottom and a
    /// title row (~12px, plus a 2px gap) under the controls.
    #[test]
    fn the_well_fits_the_ribbon_group_body() {
        assert!(well_height() <= 98. - 4. - 14., "{}", well_height());
    }

    #[test]
    fn mix_matches_css_color_mix() {
        let white = gpui::rgb(0xffffff);
        let black = gpui::rgb(0x000000);
        let m = mix(white, black, 25.);
        assert!((m.r - 0.75).abs() < 1e-6 && (m.b - 0.75).abs() < 1e-6);
        assert_eq!(mix(white, black, 0.), white);
    }

    #[test]
    fn contrast_matches_wcag_reference_points() {
        let r = contrast(gpui::rgb(0x000000), gpui::rgb(0xffffff));
        assert!((r - 21.).abs() < 0.01, "{r}");
        // #767676 on white is the classic 4.54:1.
        let r = contrast(gpui::rgb(0x767676), gpui::rgb(0xffffff));
        assert!((r - 4.54).abs() < 0.01, "{r}");
    }
}
