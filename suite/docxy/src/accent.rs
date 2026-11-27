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

use crate::{BRAND, FILE_FG, Kind, ext_kinds};

static FILL: AtomicU32 = AtomicU32::new(BRAND);
static ON_FILL: AtomicU32 = AtomicU32::new(FILE_FG);
static MARK: AtomicU32 = AtomicU32::new(BRAND);
static TEXT: AtomicU32 = AtomicU32::new(BRAND);
static CHROME_FILL: AtomicU32 = AtomicU32::new(BRAND);
static CHROME_ON_FILL: AtomicU32 = AtomicU32::new(FILE_FG);
static CHROME_MARK: AtomicU32 = AtomicU32::new(BRAND);
static CHROME_TEXT: AtomicU32 = AtomicU32::new(BRAND);

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
/// suite takes ([`crate::brand::accents`]).
pub(crate) fn drawn(accent: u32, plugin: &crate::brand::Accents) -> Drawn {
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

/// Record the brand's roles this window is about to draw ([`chrome`]).
pub(crate) fn set_chrome(d: Drawn) {
    CHROME_FILL.store(d.fill, Ordering::Relaxed);
    CHROME_ON_FILL.store(d.on_fill, Ordering::Relaxed);
    CHROME_MARK.store(d.mark, Ordering::Relaxed);
    CHROME_TEXT.store(d.text, Ordering::Relaxed);
}

/// The brand's roles now, whichever tab is selected: the chrome outside
/// the ribbon, backstage and title bar (dialogs, panes, bars, fields)
/// draws these, so a sheet tab's dialog keeps the brand as before and a
/// plugin's colours reach it. Without a plugin each is `BRAND` (on_fill
/// `FILE_FG`).
pub(crate) fn chrome() -> Drawn {
    Drawn {
        fill: CHROME_FILL.load(Ordering::Relaxed),
        on_fill: CHROME_ON_FILL.load(Ordering::Relaxed),
        mark: CHROME_MARK.load(Ordering::Relaxed),
        text: CHROME_TEXT.load(Ordering::Relaxed),
    }
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
        let none = crate::brand::Accents::default();
        let own = Drawn {
            fill: BRAND,
            on_fill: FILE_FG,
            mark: BRAND,
            text: BRAND,
        };
        assert_eq!(drawn(BRAND, &none), own);
        let plugin = crate::brand::Accents {
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
        let only_link = crate::brand::Accents {
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

    /// The `BRAND` uses outside this file that keep the brand's own teal
    /// and take no role of [`chrome`]: (file, the item they are in, how
    /// many, why). Every other accent the chrome draws (focus rings,
    /// checked, chosen and active marks, selection washes, primary buttons,
    /// accent text) takes a role, so a plugin's colours reach it.
    const BRAND_KEPT: &[(&str, &str, usize, &str)] = &[
        ("main.rs", "BRAND", 1, "the brand itself"),
        (
            "main.rs",
            "build",
            1,
            "the first frame's accent, before render sets it",
        ),
        (
            "main.rs",
            "render",
            2,
            "the brand the roles start from; the ruler's guide on the page",
        ),
        ("main.rs", "caret_bar_h", 1, "a caret"),
        ("main.rs", "fx_edit_row", 1, "a caret"),
        ("main.rs", "ref_field_row", 1, "a caret"),
        ("main.rs", "sheet_combo", 1, "a caret"),
        ("main.rs", "sheet_cf_bar", 1, "a caret"),
        ("main.rs", "sheet_rowh_bar", 1, "a caret"),
        ("main.rs", "sheet_comment_bar", 1, "a caret"),
        ("main.rs", "sheet_find_bar", 2, "carets"),
        ("main.rs", "combo_box", 1, "a caret"),
        ("main.rs", "info_properties", 1, "a caret"),
        (
            "main.rs",
            "sheet_el",
            3,
            "carets; a chart's selection frame on the grid",
        ),
        ("project.rs", "line_edit_view", 1, "a caret"),
        ("main.rs", "comment_bar", 3, "comments"),
        ("main.rs", "comments_panel", 3, "comments"),
        ("main.rs", "notes_panel", 1, "comments: a note's tag"),
        ("main.rs", "paragraph_el", 3, "document content"),
        (
            "table_tab.rs",
            "draw_overlay",
            1,
            "document content: the table pen's ink",
        ),
        (
            "main.rs",
            "sheet_row",
            3,
            "the sheet's grid: selection, range tint, fill handle",
        ),
        (
            "main.rs",
            "chart_grips",
            1,
            "the sheet's grid: a chart's grips",
        ),
        (
            "main.rs",
            "picture_grips",
            1,
            "the sheet's grid: a picture's grips",
        ),
        (
            "main.rs",
            "sheet_picture",
            1,
            "the sheet's grid: a picture's frame",
        ),
        (
            "main.rs",
            "pivot_panel",
            1,
            "the Rows area's colour, beside Columns' and Values'",
        ),
        (
            "main.rs",
            "ref_field",
            1,
            "a valid reference's status, beside the invalid red",
        ),
        (
            "project/gantt.rs",
            "bar_kind_color",
            1,
            "Project's Gantt bars",
        ),
        ("project/barstyle.rs", "of", 1, "Project's Gantt bar styles"),
        (
            "ribbon_export.rs",
            "docx_snapshot",
            1,
            "the ribbon export's data",
        ),
    ];

    /// The item a line of code begins: its first `fn name`, or a `const` or
    /// `static` at the line's start.
    fn item_named(code: &str) -> Option<&str> {
        fn ident(s: &str) -> Option<&str> {
            let n = s
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(s.len());
            (n > 0 && !s.starts_with(|c: char| c.is_ascii_digit())).then(|| &s[..n])
        }
        let mut from = 0;
        while let Some(k) = code[from..].find("fn ").map(|k| from + k) {
            if k == 0 || code.as_bytes()[k - 1].is_ascii_whitespace() {
                if let Some(name) = ident(&code[k + 3..]) {
                    return Some(name);
                }
            }
            from = k + 3;
        }
        let bare = ["pub(crate) ", "pub(super) ", "pub "]
            .iter()
            .find_map(|p| code.strip_prefix(p))
            .unwrap_or(code);
        ["const ", "static "]
            .iter()
            .find_map(|kw| bare.strip_prefix(kw).and_then(ident))
    }

    /// How many times `code` names `BRAND` as a whole word.
    fn brand_words(code: &str) -> usize {
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let b = code.as_bytes();
        code.match_indices("BRAND")
            .filter(|(k, _)| (*k == 0 || !word(b[k - 1])) && b.get(k + 5).is_none_or(|&c| !word(c)))
            .count()
    }

    /// The `BRAND` uses of `src` outside comments and test items, by the
    /// item they are in.
    fn brand_uses_in(src: &str) -> Vec<(String, usize)> {
        let mut found: Vec<(String, usize)> = Vec::new();
        let mut item = String::new();
        for line in crate::brand::tests::without_test_items(src).lines() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(name) = item_named(code) {
                item = name.to_string();
            }
            let n = brand_words(code);
            if n == 0 {
                continue;
            }
            match found.iter_mut().find(|(i, _)| *i == item) {
                Some((_, c)) => *c += n,
                None => found.push((item.clone(), n)),
            }
        }
        found
    }

    /// Each `BRAND` use of the suite's non-test source outside this file:
    /// (file, item, how many), sorted.
    fn brand_uses(root: &std::path::Path) -> Vec<(String, String, usize)> {
        let mut found = Vec::new();
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() {
                    if name != "tests" {
                        dirs.push(path);
                    }
                    continue;
                }
                if !name.ends_with(".rs") || name.ends_with("tests.rs") {
                    continue;
                }
                let file = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if file == "accent.rs" {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                for (item, n) in brand_uses_in(&src) {
                    found.push((file.clone(), item, n));
                }
            }
        }
        found.sort();
        found
    }

    /// The chrome draws the brand's roles, not `BRAND`: the uses left are
    /// exactly [`BRAND_KEPT`]'s, so a new one is decided here (a role, or
    /// kept with its reason) and a kept one that went is struck.
    #[test]
    fn the_chrome_draws_the_brands_roles_and_brand_is_kept_only_here() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut kept: Vec<(String, String, usize)> = BRAND_KEPT
            .iter()
            .map(|(f, i, n, _)| (f.to_string(), i.to_string(), *n))
            .collect();
        kept.sort();
        assert_eq!(
            brand_uses(&root),
            kept,
            "draw these through accent::chrome(), or keep them in BRAND_KEPT"
        );
    }

    /// The guard's scanner: a use in code counts, by its item; one in a
    /// comment, a test item (a test field, variant or match arm among them)
    /// or a longer name does not.
    #[test]
    fn the_brand_scan_counts_code_by_item_and_not_comments_tests_or_longer_names() {
        let src = concat!(
            "const BRAND: u32 = 1;\n",
            "/// `BRAND` in a doc comment\n",
            "fn a() -> u32 {\n",
            "    let x = BRAND; // BRAND after code\n",
            "    x + crate::BRAND + BRAND_X + XBRAND\n",
            "}\n",
            "pub(crate) fn b() { f(hsla_u(BRAND)); }\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    fn t() { BRAND; }\n",
            "}\n",
            "struct S {\n",
            "    #[cfg(test)]\n",
            "    t: u32,\n",
            "}\n",
            "fn d() -> u32 { BRAND }\n",
            "enum V {\n",
            "    #[cfg(test)]\n",
            "    T,\n",
            "}\n",
            "fn e() -> u32 { BRAND }\n",
            "fn g(x: u8) -> u32 {\n",
            "    match x {\n",
            "        #[cfg(test)]\n",
            "        0 => BRAND,\n",
            "        _ => BRAND,\n",
            "    }\n",
            "}\n",
            "fn c() {}\n",
        );
        assert_eq!(
            brand_uses_in(src),
            [
                ("BRAND".to_string(), 1),
                ("a".to_string(), 2),
                ("b".to_string(), 1),
                ("d".to_string(), 1),
                ("e".to_string(), 1),
                ("g".to_string(), 1),
            ]
        );
    }

    /// With no plugin the brand's roles are `BRAND` (text on the fill
    /// `FILE_FG`), so the chrome draws as before, whichever tab is
    /// selected: they start from the brand, not the tab's accent.
    #[test]
    fn the_brands_roles_are_brand_without_a_plugin() {
        let own = drawn(BRAND, &crate::brand::Accents::default());
        assert_eq!(
            own,
            Drawn {
                fill: BRAND,
                on_fill: FILE_FG,
                mark: BRAND,
                text: BRAND,
            }
        );
    }
}
