//! The accent of the selected tab's type (#1473): doc keeps the brand teal
//! (cyan), sheet is green and project yellow, as in the terminal apps. The
//! title label names the type in one word.
//!
//! `Docxy::render` calls [`set`] once before it builds any chrome, and the
//! ribbon, backstage and title bar read [`brand`], so no builder takes a
//! colour. Windows render one at a time, each setting it first.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::{BRAND, Kind};

static CURRENT: AtomicU32 = AtomicU32::new(BRAND);

/// The accent for `kind` (None: no tab) on the light or dark theme. Both
/// shades hold white File-button text at 4.5:1, except doc, whose teal is the
/// unchanged brand.
pub(crate) fn for_kind(kind: Option<Kind>, dark: bool) -> u32 {
    match (kind, dark) {
        (Some(Kind::Xlsx), false) => 0x1E7B3A,
        (Some(Kind::Xlsx), true) => 0x23843F,
        (Some(Kind::Project), false) => 0x8F6B00,
        (Some(Kind::Project), true) => 0x946F00,
        _ => BRAND,
    }
}

/// The one word the title bar shows for the selected tab.
pub(crate) fn label(kind: Option<Kind>) -> &'static str {
    match kind {
        Some(Kind::Xlsx) => "sheet",
        Some(Kind::Project) => "project",
        Some(Kind::Look) => "mail",
        Some(Kind::Docx) | None => "doc",
    }
}

/// Record the accent this window is about to draw.
pub(crate) fn set(color: u32) {
    CURRENT.store(color, Ordering::Relaxed);
}

/// The accent the chrome draws now.
pub(crate) fn brand() -> u32 {
    CURRENT.load(Ordering::Relaxed)
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
