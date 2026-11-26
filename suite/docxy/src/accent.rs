//! The accent of the selected tab's type (#1473): doc keeps the brand teal
//! (cyan), sheet is green and project yellow, as in the terminal apps. The
//! title label names the type in one word.
//!
//! `Docxy::render` calls [`set`] once before it builds any chrome, and the
//! ribbon, backstage and title bar read [`fill`], [`on_fill`], [`mark`] and
//! [`text`], so no builder takes a colour. Windows render one at a time,
//! each setting it first.

use std::sync::atomic::{AtomicU32, Ordering};

use docxy_integration::protocol::Rgb;

use crate::{BRAND, FILE_FG, Kind, brand, ext_kinds};

static FILL: AtomicU32 = AtomicU32::new(BRAND);
static ON_FILL: AtomicU32 = AtomicU32::new(FILE_FG);
static MARK: AtomicU32 = AtomicU32::new(BRAND);
static TEXT: AtomicU32 = AtomicU32::new(BRAND);

/// The colours the chrome draws for one accent, by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Drawn {
    /// Fills with text on them, washes, the focus ring.
    pub(crate) fill: u32,
    /// The text on [`Drawn::fill`].
    pub(crate) on_fill: u32,
    /// Small marks: the active tab's, a selected tile's, a hover border.
    pub(crate) mark: u32,
    /// Accent text on the background.
    pub(crate) text: u32,
}

/// The roles of `accent` ([`for_tab`]): `accent` in each, white text on its
/// fill, except that the brand takes each colour the plugin gives that the
/// suite takes ([`brand::accents`]).
pub(crate) fn drawn(accent: u32, plugin: &brand::Accents) -> Drawn {
    let mut d = Drawn {
        fill: accent,
        on_fill: FILE_FG,
        mark: accent,
        text: accent,
    };
    if accent == BRAND {
        if let Some(p) = plugin.primary {
            d.fill = p.color.to_u32();
            d.on_fill = p.text.map_or(FILE_FG, Rgb::to_u32);
        }
        if let Some(h) = plugin.highlight {
            d.mark = h.to_u32();
        }
        if let Some(l) = plugin.link {
            d.text = l.to_u32();
        }
    }
    d
}

/// The accent for `kind` (None: no tab) on the light or dark theme. Both
/// shades hold white File-button text at 4.5:1, except doc, whose teal is the
/// unchanged brand. A further extension kind (#2253) is the brand here: its
/// own accent is [`for_tab`]'s.
pub(crate) fn for_kind(kind: Option<Kind>, dark: bool) -> u32 {
    match (kind, dark) {
        (Some(Kind::Xlsx), false) => 0x1E7B3A,
        (Some(Kind::Xlsx), true) => 0x23843F,
        (Some(Kind::Project), false) => 0x8F6B00,
        (Some(Kind::Project), true) => 0x946F00,
        _ => BRAND,
    }
}

/// [`for_kind`], with a further extension kind's own accent (#2253) when
/// this App registers it with one.
pub(crate) fn for_tab(kind: Option<Kind>, dark: bool, cx: &gpui::App) -> u32 {
    for_tab_in(kind, dark, |id| ext_kinds::find(id, cx))
}

/// [`for_tab`] with `find` as the registry: an Ext kind it finds with an
/// accent takes that accent's shade; one it does not, or one with none, is
/// the brand.
fn for_tab_in<'a>(
    kind: Option<Kind>,
    dark: bool,
    find: impl Fn(ext_kinds::ExtId) -> Option<&'a ext_kinds::Registered>,
) -> u32 {
    match kind {
        Some(Kind::Ext(id)) => find(id)
            .and_then(|r| r.accent)
            .map_or(BRAND, |(light, shade)| if dark { shade } else { light }),
        k => for_kind(k, dark),
    }
}

/// [`label`], with a registered extension kind's own word (#2253): its
/// name, lowercased.
pub(crate) fn label_for(kind: Option<Kind>, cx: &gpui::App) -> &'static str {
    label_in(kind, |id| ext_kinds::find(id, cx))
}

/// [`label_for`] with `find` as the registry: an Ext kind it does not find
/// is its id.
fn label_in<'a>(
    kind: Option<Kind>,
    find: impl Fn(ext_kinds::ExtId) -> Option<&'a ext_kinds::Registered>,
) -> &'static str {
    match kind {
        Some(Kind::Ext(id)) => find(id).map_or(id.as_str(), |r| r.label),
        k => label(k),
    }
}

/// The one word the title bar shows for the selected tab. A further
/// extension kind (#2253) shows its id here; [`label_for`] its name.
pub(crate) fn label(kind: Option<Kind>) -> &'static str {
    match kind {
        Some(Kind::Ext(id)) => id.as_str(),
        Some(Kind::Xlsx) => "sheet",
        Some(Kind::Project) => "project",
        Some(Kind::Look) => "mail",
        Some(Kind::Help) => "help",
        Some(Kind::Pdf) => "pdf",
        Some(Kind::Docx) | None => "doc",
    }
}

/// Record the accent this window is about to draw.
pub(crate) fn set(d: Drawn) {
    FILL.store(d.fill, Ordering::Relaxed);
    ON_FILL.store(d.on_fill, Ordering::Relaxed);
    MARK.store(d.mark, Ordering::Relaxed);
    TEXT.store(d.text, Ordering::Relaxed);
}

/// [`Drawn::fill`] now.
pub(crate) fn fill() -> u32 {
    FILL.load(Ordering::Relaxed)
}

/// [`Drawn::on_fill`] now.
pub(crate) fn on_fill() -> u32 {
    ON_FILL.load(Ordering::Relaxed)
}

/// [`Drawn::mark`] now.
pub(crate) fn mark() -> u32 {
    MARK.load(Ordering::Relaxed)
}

/// [`Drawn::text`] now.
pub(crate) fn text() -> u32 {
    TEXT.load(Ordering::Relaxed)
}

/// The fill as a translucent wash of opacity `alpha`, the project
/// table's selection colours: it reads on the light and the dark theme.
pub(crate) fn wash(alpha: f32) -> gpui::Hsla {
    let c: gpui::Hsla = gpui::rgb(fill()).into();
    gpui::hsla(c.h, c.s, c.l, alpha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style_gallery::contrast;
    use gpui_component::theme::ThemeColor;

    const KINDS: [Option<Kind>; 5] = [
        None,
        Some(Kind::Docx),
        Some(Kind::Xlsx),
        Some(Kind::Project),
        Some(Kind::Look),
    ];

    #[test]
    fn the_brand_takes_the_plugins_roles_and_a_types_accent_stays() {
        use docxy_integration::accent::Accepted;
        let none = brand::Accents::default();
        let own = Drawn {
            fill: BRAND,
            on_fill: FILE_FG,
            mark: BRAND,
            text: BRAND,
        };
        assert_eq!(drawn(BRAND, &none), own);
        let plugin = brand::Accents {
            primary: Some(Accepted {
                color: Rgb(0x11, 0x22, 0x33),
                text: Some(Rgb(0, 0, 0)),
            }),
            link: Some(Rgb(0x44, 0x55, 0x66)),
            highlight: Some(Rgb(0x77, 0x88, 0x99)),
        };
        assert_eq!(
            drawn(BRAND, &plugin),
            Drawn {
                fill: 0x112233,
                on_fill: 0x000000,
                mark: 0x778899,
                text: 0x445566,
            }
        );
        let only_link = brand::Accents {
            link: Some(Rgb(0x44, 0x55, 0x66)),
            ..none
        };
        assert_eq!(
            drawn(BRAND, &only_link),
            Drawn {
                text: 0x445566,
                ..own
            }
        );
        for dark in [false, true] {
            let sheet = for_kind(Some(Kind::Xlsx), dark);
            assert_eq!(
                drawn(sheet, &plugin),
                Drawn {
                    fill: sheet,
                    on_fill: FILE_FG,
                    mark: sheet,
                    text: sheet,
                }
            );
        }
    }

    #[test]
    fn the_label_names_the_type() {
        assert_eq!(label(Some(Kind::Docx)), "doc");
        assert_eq!(label(Some(Kind::Xlsx)), "sheet");
        assert_eq!(label(Some(Kind::Project)), "project");
        assert_eq!(label(Some(Kind::Look)), "mail");
        assert_eq!(label(None), "doc");
    }

    #[test]
    fn doc_mail_and_no_tab_keep_the_brand() {
        for dark in [false, true] {
            for k in [None, Some(Kind::Docx), Some(Kind::Look)] {
                assert_eq!(for_kind(k, dark), BRAND);
            }
        }
    }

    #[test]
    fn each_type_has_its_own_accent_per_theme() {
        for dark in [false, true] {
            let a: Vec<u32> = [Kind::Docx, Kind::Xlsx, Kind::Project]
                .iter()
                .map(|k| for_kind(Some(*k), dark))
                .collect();
            assert!(a[0] != a[1] && a[1] != a[2] && a[0] != a[2], "{a:x?}");
        }
        assert_ne!(
            for_kind(Some(Kind::Xlsx), false),
            for_kind(Some(Kind::Xlsx), true)
        );
        assert_ne!(
            for_kind(Some(Kind::Project), false),
            for_kind(Some(Kind::Project), true)
        );
    }

    /// #2253: a further extension kind this App does not register is the
    /// brand and its id; one it registers is its own accent's shade and its
    /// name lowercased; one registered with no accent is the brand.
    #[test]
    fn an_extension_kind_is_its_registrations_accent_and_label() {
        let (kinds, _) = ext_kinds::validate(vec![
            crate::ext_fixture::kind(false),
            crate::ext::ExtKind::new(
                "plain",
                "Plain Notes",
                "",
                "",
                crate::ext_fixture::factory(false),
            ),
        ]);
        let find = |id| kinds.iter().find(|r| r.id == id);
        let nothing = |_| None;
        let gone = Some(Kind::Ext(ext_kinds::ExtId::intern("some.kind")));
        let fixture = Some(Kind::Ext(ext_kinds::ExtId::intern("fixture")));
        let plain = Some(Kind::Ext(ext_kinds::ExtId::intern("plain")));
        assert_eq!(label(gone), "some.kind");
        assert_eq!(label_in(gone, find), "some.kind");
        assert_eq!(label_in(fixture, nothing), "fixture");
        assert_eq!(label_in(fixture, find), "fixture");
        assert_eq!(label_in(plain, find), "plain notes");
        assert_eq!(label_in(Some(Kind::Xlsx), find), "sheet");
        let (light, dark_shade) = crate::ext_fixture::KIND_ACCENT;
        for dark in [false, true] {
            assert_eq!(for_kind(fixture, dark), BRAND);
            assert_eq!(for_tab_in(gone, dark, find), BRAND);
            assert_eq!(for_tab_in(fixture, dark, nothing), BRAND);
            assert_eq!(for_tab_in(plain, dark, find), BRAND);
            assert_eq!(
                for_tab_in(Some(Kind::Project), dark, find),
                for_kind(Some(Kind::Project), dark)
            );
        }
        assert_eq!(for_tab_in(fixture, false, find), light);
        assert_eq!(for_tab_in(fixture, true, find), dark_shade);
    }

    /// The harness's test kind's accent (#2253) holds the same bar as the
    /// built-in kinds', and is none of theirs.
    #[test]
    fn the_fixture_kinds_accent_reads_and_is_its_own() {
        let (light, dark_shade) = crate::ext_fixture::KIND_ACCENT;
        for (dark, a) in [(false, light), (true, dark_shade)] {
            let bg = if dark {
                ThemeColor::dark().background
            } else {
                ThemeColor::light().background
            }
            .to_rgb();
            let on_bg = contrast(gpui::rgb(a), bg);
            assert!(on_bg >= 3.0, "dark={dark} on background: {on_bg:.2}");
            let w = contrast(gpui::rgb(crate::FILE_FG), gpui::rgb(a));
            assert!(w >= 4.5, "dark={dark} under File text: {w:.2}");
            for k in KINDS {
                assert_ne!(a, for_kind(k, dark));
            }
        }
    }

    #[test]
    fn accents_read_on_the_theme_and_under_white_file_text() {
        for dark in [false, true] {
            let bg = if dark {
                ThemeColor::dark().background
            } else {
                ThemeColor::light().background
            }
            .to_rgb();
            for k in KINDS {
                // Doc, mail and no tab keep today's brand teal unchanged.
                if matches!(k, Some(Kind::Xlsx | Kind::Project)) {
                    let a = gpui::rgb(for_kind(k, dark));
                    let on_bg = contrast(a, bg);
                    assert!(
                        on_bg >= 3.0,
                        "{} dark={dark} on background: {on_bg:.2}",
                        label(k)
                    );
                    let w = contrast(gpui::rgb(crate::FILE_FG), a);
                    assert!(w >= 4.5, "{} dark={dark} under File text: {w:.2}", label(k));
                }
            }
        }
    }
}
