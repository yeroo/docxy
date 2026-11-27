//! The product's names and icon as the suite shows them: its integration
//! plugin's (`docs/integration-plugins.md`), when it gives them, else the
//! suite's own.

use docxy_integration::accent::{self as rule, Role};
use docxy_integration::host::Status;
use docxy_integration::protocol::{AccentSet, App, Branding, Context, Rgb};
use std::sync::OnceLock;

/// The suite's own name in About.
pub(crate) const PRODUCT: &str = "docxy suite";
/// The suite's own name in window and message titles.
pub(crate) const SHORT: &str = "docxy";

/// The name in About: the plugin's `product`, else [`PRODUCT`].
pub(crate) fn product_of(branding: Option<&Branding>) -> &str {
    branding
        .and_then(|b| b.product.as_deref())
        .unwrap_or(PRODUCT)
}

/// The name in window and message titles: the plugin's `short`, else its
/// `product`, else [`SHORT`].
pub(crate) fn short_of(branding: Option<&Branding>) -> &str {
    branding
        .and_then(|b| b.short.as_deref().or(b.product.as_deref()))
        .unwrap_or(SHORT)
}

/// What the About commands say.
pub(crate) fn about_label_of(branding: Option<&Branding>) -> String {
    format!("About {}", product_of(branding))
}

/// The suite's branding, asked of the process's plugin once.
fn branding() -> Option<&'static Branding> {
    static BRANDING: OnceLock<Option<Branding>> = OnceLock::new();
    BRANDING
        .get_or_init(|| docxy_integration::global().branding(App::Suite))
        .as_ref()
}

pub(crate) fn short() -> &'static str {
    short_of(branding())
}

/// [`about_label_of`] for this run, for the ribbon's static labels.
pub(crate) fn about_label() -> &'static str {
    static LABEL: OnceLock<String> = OnceLock::new();
    LABEL.get_or_init(|| about_label_of(branding()))
}

/// The plugin's line for About.
pub(crate) fn about_line() -> Option<&'static str> {
    branding().and_then(|b| b.about_line.as_deref())
}

/// The plugin's `.ico`, for each window's icon.
#[cfg(windows)]
fn icon_ico() -> Option<&'static [u8]> {
    branding().and_then(|b| b.icon_ico.as_deref())
}

/// The rows About adds for the integration: its line, the organization and
/// desktop it reports, and its state. Empty with no plugin configured.
pub(crate) fn about_rows_of(
    status: &Status,
    about_line: Option<&str>,
    context: Option<&Context>,
) -> Vec<(&'static str, &'static str, String)> {
    let mut rows = Vec::new();
    match status {
        Status::NotConfigured => return rows,
        Status::Loaded { file, version } => {
            if let Some(line) = about_line {
                rows.push(("integration line", "", line.to_string()));
            }
            let named =
                |n: &docxy_integration::protocol::Named| n.name.clone().or_else(|| n.id.clone());
            if let Some(org) = context
                .and_then(|c| c.organization.as_ref())
                .and_then(named)
            {
                rows.push(("organization", "Organization", org));
            }
            if let Some(desk) = context.and_then(|c| c.desktop.as_ref()).and_then(named) {
                rows.push(("desktop", "Desktop", desk));
            }
            rows.push(("integration", "Integration", format!("{file} {version}")));
        }
        Status::Refused { why, .. } => {
            rows.push(("integration", "Integration", format!("not loaded: {why}")));
        }
    }
    rows
}

/// [`about_rows_of`] for this run.
pub(crate) fn about_rows() -> Vec<(&'static str, &'static str, String)> {
    let integration = docxy_integration::global();
    let status = integration.status();
    let context = match status {
        Status::Loaded { .. } => integration.context(),
        _ => None,
    };
    about_rows_of(&status, about_line(), context.as_ref())
}

/// `app-info`'s `integration`: its state, and the organization and desktop
/// it reports.
pub(crate) fn app_info_json() -> ctlcore::json::Json {
    use ctlcore::json::Json;
    let integration = docxy_integration::global();
    let text = |t: Option<&str>| t.map_or(Json::Null, |t| Json::Str(t.to_string()));
    let named = |n: Option<&docxy_integration::protocol::Named>| {
        n.map_or(Json::Null, |n| {
            Json::obj(vec![
                ("id", text(n.id.as_deref())),
                ("name", text(n.name.as_deref())),
            ])
        })
    };
    match integration.status() {
        Status::NotConfigured => Json::obj(vec![("status", Json::Str("not configured".into()))]),
        Status::Refused { file, why } => Json::obj(vec![
            ("status", Json::Str("refused".into())),
            ("file", Json::Str(file)),
            ("reason", Json::Str(why)),
        ]),
        Status::Loaded { file, version } => {
            let context = integration.context();
            Json::obj(vec![
                ("status", Json::Str("loaded".into())),
                ("file", Json::Str(file)),
                ("version", Json::Str(version)),
                (
                    "organization",
                    named(context.as_ref().and_then(|c| c.organization.as_ref())),
                ),
                (
                    "desktop",
                    named(context.as_ref().and_then(|c| c.desktop.as_ref())),
                ),
            ])
        }
    }
}

/// Give `window` the plugin's icon, when it gives one. Windows only: on the
/// other platforms gpui sets no window icon from the exe either.
pub(crate) fn set_window_icon(window: &gpui::Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Some(ico) = icon_ico() else {
            return;
        };
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        if let RawWindowHandle::Win32(win32) = handle.as_raw()
            && let Err(e) = docxy_integration::win_icon::set_window_icon(win32.hwnd.get(), ico)
        {
            eprintln!("docxy: the integration plugin's icon: {e}");
        }
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// The plugin's accent colours the suite draws on one theme: each that
/// passes [`rule::check`] for its role, `None` where the suite keeps its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Accents {
    pub(crate) primary: Option<rule::Accepted>,
    pub(crate) link: Option<Rgb>,
    pub(crate) highlight: Option<Rgb>,
}

/// [`Accents`] of `set` on a theme whose background is `background`, and
/// why each colour it gives and the suite does not take was refused.
pub(crate) fn accents_of(set: Option<AccentSet>, background: Rgb) -> (Accents, Vec<String>) {
    let set = set.unwrap_or_default();
    let mut refused = Vec::new();
    let mut take = |role, color: Option<Rgb>| {
        rule::check(role, color?, background)
            .map_err(|why| refused.push(why))
            .ok()
    };
    let accents = Accents {
        primary: take(Role::Primary, set.primary),
        link: take(Role::Link, set.link).map(|a| a.color),
        highlight: take(Role::Highlight, set.highlight).map(|a| a.color),
    };
    (accents, refused)
}

/// [`accents_of`] the plugin's set for the dark theme when `dark`, else the
/// light one, on that theme's background, worked out once per theme. Each
/// refusal goes to stderr then.
pub(crate) fn accents(dark: bool) -> Accents {
    static THEMES: [OnceLock<Accents>; 2] = [OnceLock::new(), OnceLock::new()];
    *THEMES[usize::from(dark)].get_or_init(|| {
        let set = branding()
            .and_then(|b| b.accents)
            .and_then(|a| a.for_theme(dark));
        let background = if dark {
            gpui_component::theme::ThemeColor::dark().background
        } else {
            gpui_component::theme::ThemeColor::light().background
        };
        let (accents, refused) = accents_of(set, Rgb::from_u32(crate::rgb_u(background)));
        let theme = if dark { "dark" } else { "light" };
        for why in refused {
            eprintln!(
                "docxy: the integration plugin's {theme} theme: {why}; the suite keeps its own"
            );
        }
        accents
    })
}

/// The plugin's [`accents`] on the global theme's tokens, run after every
/// `Theme::change`.
pub(crate) fn apply_accents_to_theme(cx: &mut gpui::App) {
    let dark = gpui_component::Theme::global(cx).mode.is_dark();
    let accents = accents(dark);
    let theme = gpui_component::Theme::global_mut(cx);
    if let Some(primary) = accents.primary {
        let c = crate::hsla_u(primary.color.to_u32());
        theme.primary = c;
        theme.primary_hover = c;
        theme.primary_active = c;
        theme.ring = c;
        theme.selection = gpui::Hsla {
            a: theme.selection.a,
            ..c
        };
        if let Some(text) = primary.text {
            theme.primary_foreground = crate::hsla_u(text.to_u32());
        }
    }
    if let Some(link) = accents.link {
        let c = crate::hsla_u(link.to_u32());
        theme.link = c;
        theme.link_hover = c;
        theme.link_active = c;
    }
}

/// The plugin's refusal or stop said at start, kept for the first window's
/// status line.
static STARTUP_NOTICE: OnceLock<Option<String>> = OnceLock::new();

/// Start the integration plugin for this process (before anything reads its
/// branding), ask its branding and context once, and return what it has to
/// say, for stderr and the crash log. A harness instance loads none.
pub(crate) fn start(config_root: &std::path::Path, harness: bool) -> Option<String> {
    if !harness {
        let config = docxy_integration::host::HostConfig::new(
            App::Suite,
            env!("CARGO_PKG_VERSION"),
            Some(config_root.join("docxy")),
        );
        let integration = docxy_integration::start(config);
        let _ = branding();
        let _ = integration.context();
    }
    let notice = docxy_integration::global().take_notice();
    STARTUP_NOTICE.get_or_init(|| notice.clone());
    notice
}

/// The notice [`start`] returned, for the first window's status line.
pub(crate) fn startup_notice() -> Option<&'static str> {
    STARTUP_NOTICE.get().and_then(|n| n.as_deref())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use docxy_integration::protocol::Named;

    fn branding(product: Option<&str>, short: Option<&str>) -> Branding {
        Branding {
            product: product.map(str::to_string),
            short: short.map(str::to_string),
            ..Branding::default()
        }
    }

    #[test]
    fn with_no_branding_the_suite_keeps_its_names() {
        assert_eq!(product_of(None), "docxy suite");
        assert_eq!(short_of(None), "docxy");
        assert_eq!(about_label_of(None), "About docxy suite");
        let empty = Branding::default();
        assert_eq!(product_of(Some(&empty)), "docxy suite");
        assert_eq!(short_of(Some(&empty)), "docxy");
    }

    #[test]
    fn a_plugins_names_replace_the_suites() {
        let both = branding(Some("Product Suite"), Some("Product"));
        assert_eq!(product_of(Some(&both)), "Product Suite");
        assert_eq!(short_of(Some(&both)), "Product");
        assert_eq!(about_label_of(Some(&both)), "About Product Suite");
        let product_only = branding(Some("Product Suite"), None);
        assert_eq!(short_of(Some(&product_only)), "Product Suite");
        let short_only = branding(None, Some("Product"));
        assert_eq!(product_of(Some(&short_only)), "docxy suite");
        assert_eq!(short_of(Some(&short_only)), "Product");
    }

    #[test]
    fn the_plugins_accents_are_each_held_to_their_role_on_the_themes_background() {
        let grey = |v| Rgb(v, v, v);
        let (white, black) = (Rgb(0xFF, 0xFF, 0xFF), Rgb(0, 0, 0));
        assert_eq!(accents_of(None, white), (Accents::default(), vec![]));
        let set = AccentSet {
            primary: Some(Rgb(0x10, 0x20, 0x80)),
            link: Some(grey(0x77)),
            highlight: Some(grey(0x76)),
        };
        let (on_white, refused) = accents_of(Some(set), white);
        assert_eq!(
            on_white,
            Accents {
                primary: Some(rule::Accepted {
                    color: Rgb(0x10, 0x20, 0x80),
                    text: Some(white),
                }),
                link: None,
                highlight: Some(grey(0x76)),
            }
        );
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("link accent #777777"), "{refused:?}");
        let (on_black, refused) = accents_of(Some(set), black);
        assert_eq!(
            (on_black.link, on_black.highlight),
            (Some(grey(0x77)), Some(grey(0x76)))
        );
        assert!(refused.is_empty(), "{refused:?}");
        let (only_highlight, _) = accents_of(
            Some(AccentSet {
                highlight: Some(grey(0x40)),
                ..AccentSet::default()
            }),
            black,
        );
        assert_eq!(only_highlight, Accents::default());
    }

    #[test]
    fn about_shows_the_integration_only_when_one_is_configured() {
        assert!(about_rows_of(&Status::NotConfigured, Some("line"), None).is_empty());
        let refused = Status::Refused {
            file: "dxi_x.dll".into(),
            why: "it speaks interface version 0; this host takes 1 to 1".into(),
        };
        assert_eq!(
            about_rows_of(&refused, None, None),
            [(
                "integration",
                "Integration",
                "not loaded: it speaks interface version 0; this host takes 1 to 1".to_string()
            )]
        );
        let loaded = Status::Loaded {
            file: "dxi_x.dll".into(),
            version: "1.0.0".into(),
        };
        let context = Context {
            organization: Some(Named {
                id: Some("org-1".into()),
                name: Some("Org".into()),
            }),
            desktop: Some(Named {
                id: Some("desk-1".into()),
                name: None,
            }),
        };
        assert_eq!(
            about_rows_of(&loaded, Some("A line"), Some(&context)),
            [
                ("integration line", "", "A line".to_string()),
                ("organization", "Organization", "Org".to_string()),
                ("desktop", "Desktop", "desk-1".to_string()),
                ("integration", "Integration", "dxi_x.dll 1.0.0".to_string()),
            ]
        );
    }

    /// Product-name literals the guard below lets through, each with its file
    /// and why. A literal is kept only as written here.
    const NAME_LITERALS_KEPT: &[(&str, &str, &str)] = &[
        (
            "crashlog.rs",
            "docxy suite",
            "the crash log's version block",
        ),
        (
            "crashlog.rs",
            "=== {kind} {} pid {pid} docxy {} thread {thread}\\n{build_lines}{}\\n\\n",
            "the crash log's entry line",
        ),
        (
            "main.rs",
            "failed to open docxy window",
            "a panic, in the crash log",
        ),
        ("main.rs", "io.github.yeroo.docxy", "the window's `app_id`"),
        (
            "native_dialog.rs",
            // In two parts, so `native_dialog`'s scan for the call finds no call here.
            concat!("native_", "modal( outside Docxy::native_dialog: {stray:?}"),
            "a panic, in the crash log",
        ),
        (
            "dialog.rs",
            "dialog '{}' ({}) does not match its catalogue entry: it has [{}], the catalogue lists [{}]; update suite/docxy/src/dialog/catalog/entries.rs",
            "a panic, in the crash log",
        ),
        (
            "font_subst.rs",
            "\\u{1}docxy no such font",
            "a family name no font has, to find the fallback",
        ),
        (
            "sheet_ribbon.rs",
            "About docxy suite",
            "the ribbon table's label, which `own_label` replaces",
        ),
        ("sheet_ribbon.rs", "about-docxy", "the About command's id"),
        (
            "macos_menu.rs",
            "DocxyMenuBar",
            "the menu bar's key context",
        ),
        (
            "pdf_view/worker.rs",
            "docxy-pdf-worker",
            "the PDF worker's hello",
        ),
        (
            "convert_child.rs",
            "docxy-convert-{}-{nanos}-{}",
            "a temporary file's name",
        ),
        (
            "ext_fixture.rs",
            "docxy link",
            "the harness's test extension's text",
        ),
        (
            "ext_fixture.rs",
            "https://example.com/docxy",
            "the harness's test extension's link",
        ),
        (
            "dialog/catalog/entries.rs",
            "call ribbon-click {\"tab\":\"Help\",\"command\":\"About docxy suite\"}",
            "a harness case; a harness loads no integration plugin",
        ),
        (
            "inputs.rs",
            "docxy-find",
            "a terminal docxy editor's input id",
        ),
        (
            "inputs.rs",
            "docxy-replace",
            "a terminal docxy editor's input id",
        ),
        (
            "inputs.rs",
            "docxy-comment",
            "a terminal docxy editor's input id",
        ),
        (
            "inputs.rs",
            "docxy-compare-original",
            "a terminal docxy editor's input id",
        ),
        (
            "inputs.rs",
            "docxy-compare-revised",
            "a terminal docxy editor's input id",
        ),
        (
            "inputs.rs",
            "docxy-vim-command",
            "a terminal docxy editor's input id",
        ),
        (
            "inputs.rs",
            "docxy-save-as",
            "a terminal docxy editor's input id",
        ),
        (
            "inputs.rs",
            "docxy's Find bar: Find field",
            "a terminal docxy editor's input",
        ),
        (
            "inputs.rs",
            "docxy's Find bar: Replace field",
            "a terminal docxy editor's input",
        ),
        (
            "inputs.rs",
            "docxy's new comment text",
            "a terminal docxy editor's input",
        ),
        (
            "inputs.rs",
            "docxy's Compare: original path",
            "a terminal docxy editor's input",
        ),
        (
            "inputs.rs",
            "docxy's Compare: revised path",
            "a terminal docxy editor's input",
        ),
        (
            "inputs.rs",
            "docxy's vim : command line",
            "a terminal docxy editor's input",
        ),
        (
            "inputs.rs",
            "docxy's Backstage Save As file name",
            "a terminal docxy editor's input",
        ),
        (
            "inputs.rs",
            "docxy-pdf-page",
            "a terminal docxy editor's input id",
        ),
        (
            "inputs.rs",
            "docxy's PDF viewer: Go to page",
            "a terminal docxy editor's input",
        ),
    ];

    /// Generated files' headers the guard lets through, by file and the
    /// literal's start.
    const NAME_HEADERS_KEPT: &[(&str, &str, &str)] = &[
        (
            "dialog/catalog/cases.rs",
            "# GENERATED from suite/docxy/src/dialog/catalog/entries.rs by",
            "the generated harness cases' header",
        ),
        (
            "ribbon_export.rs",
            "From suite/docxy (docxy_ribbon, table_tab, QAT_ITEMS, BACKSTAGE_RAIL, icons, theme).",
            "the ribbon export's header",
        ),
    ];

    /// Whether a literal holding the product name is kept: an environment
    /// variable (`DOCXY_`), the config folder (`.join("docxy")`), an stderr
    /// line's `docxy: ` prefix, or one of [`NAME_LITERALS_KEPT`] and
    /// [`NAME_HEADERS_KEPT`].
    fn kept_name_literal(file: &str, literal: &str, before: &str) -> bool {
        literal.starts_with("DOCXY_")
            || (literal == "docxy" && before.ends_with(".join("))
            || literal.starts_with("docxy: ")
            || NAME_LITERALS_KEPT
                .iter()
                .any(|(f, kept, _)| *f == file && literal == *kept)
            || NAME_HEADERS_KEPT
                .iter()
                .any(|(f, start, _)| *f == file && literal.starts_with(start))
    }

    /// `src` without the items under `#[cfg(test)]`: an item ends at the
    /// first line where its brackets balance and its code ends in `,`, `;`
    /// or `}`, so a test-only field, variant or match arm is one line and
    /// a test module ends at its closing brace. Brackets in strings, char
    /// literals and comments do not count.
    pub(crate) fn without_test_items(src: &str) -> String {
        let lines: Vec<&str> = src.lines().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let Some(rest) = line.trim_start().strip_prefix("#[cfg(test)]") else {
                out.push_str(line);
                out.push('\n');
                i += 1;
                continue;
            };
            i += 1;
            let mut item = TestItem::default();
            if !rest.trim().is_empty() && item.ends_after(rest) {
                continue;
            }
            while i < lines.len() && lines[i].trim_start().starts_with("#[") {
                i += 1;
            }
            while i < lines.len() {
                let ends = item.ends_after(lines[i]);
                i += 1;
                if ends {
                    break;
                }
            }
        }
        out
    }

    /// Where [`TestItem`] is in the item's text.
    #[derive(Default, Clone, Copy, PartialEq, Eq)]
    enum Lexed {
        #[default]
        Code,
        Str,
        /// A raw string with this many `#`s.
        Raw(usize),
        Block,
    }

    /// The bracket depth of a `#[cfg(test)]` item read line by line.
    #[derive(Default)]
    struct TestItem {
        depth: i32,
        at: Lexed,
    }

    impl TestItem {
        /// Read `line`; whether the item ends with it.
        fn ends_after(&mut self, line: &str) -> bool {
            let b = line.as_bytes();
            let ident = |k: usize| k > 0 && (b[k - 1].is_ascii_alphanumeric() || b[k - 1] == b'_');
            let mut last = None;
            let mut k = 0;
            while k < b.len() {
                let c = b[k];
                match self.at {
                    Lexed::Str => {
                        if c == b'\\' {
                            k += 1;
                        } else if c == b'"' {
                            self.at = Lexed::Code;
                        }
                    }
                    Lexed::Raw(hashes) => {
                        let mark = b.get(k + 1..k + 1 + hashes);
                        if c == b'"' && mark.is_some_and(|m| m.iter().all(|&h| h == b'#')) {
                            self.at = Lexed::Code;
                            k += hashes;
                        }
                    }
                    Lexed::Block => {
                        if c == b'*' && b.get(k + 1) == Some(&b'/') {
                            self.at = Lexed::Code;
                            k += 1;
                        }
                    }
                    Lexed::Code => match c {
                        b'/' if b.get(k + 1) == Some(&b'/') => break,
                        b'/' if b.get(k + 1) == Some(&b'*') => {
                            self.at = Lexed::Block;
                            k += 1;
                        }
                        b'"' => {
                            self.at = Lexed::Str;
                            last = Some(c);
                        }
                        b'r' if !ident(k) || (b[k - 1] == b'b' && !ident(k - 1)) => {
                            let hashes = b[k + 1..].iter().take_while(|&&h| h == b'#').count();
                            if b.get(k + 1 + hashes) == Some(&b'"') {
                                self.at = Lexed::Raw(hashes);
                                k += 1 + hashes;
                            }
                            last = Some(c);
                        }
                        b'\'' => {
                            // A char literal is skipped; a lifetime is not one.
                            if b.get(k + 1) == Some(&b'\\') {
                                k += 2;
                                while k < b.len() && b[k] != b'\'' {
                                    k += 1;
                                }
                            } else if let Some(len) =
                                line[k + 1..].chars().next().map(char::len_utf8)
                                && b.get(k + 1 + len) == Some(&b'\'')
                            {
                                k += 1 + len;
                            }
                            last = Some(b'\'');
                        }
                        b'{' | b'[' | b'(' => {
                            self.depth += 1;
                            last = Some(c);
                        }
                        b'}' | b']' | b')' => {
                            self.depth -= 1;
                            last = Some(c);
                        }
                        c if c.is_ascii_whitespace() => {}
                        c => last = Some(c),
                    },
                }
                k += 1;
            }
            self.at == Lexed::Code && self.depth <= 0 && matches!(last, Some(b',' | b';' | b'}'))
        }
    }

    /// The string literals of `src` outside comments, each with the source
    /// text before it: `"..."`, `b"..."` and raw strings. The text between
    /// the quotes is the source's, escapes unread.
    fn string_literals(src: &str) -> Vec<(String, String)> {
        let b = src.as_bytes();
        let ident = |k: usize| k > 0 && (b[k - 1].is_ascii_alphanumeric() || b[k - 1] == b'_');
        let mut found = Vec::new();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                        i += 1;
                    }
                    i += 2;
                }
                b'\'' => {
                    // A char literal, or a lifetime.
                    let close = (i + 2..(i + 12).min(b.len())).find(|&k| b[k] == b'\'');
                    match close {
                        Some(k)
                            if b[i + 1] == b'\\'
                                || src.get(i + 1..k).is_some_and(|c| c.chars().count() == 1) =>
                        {
                            i = k + 1
                        }
                        _ => i += 1,
                    }
                }
                b'r' if !ident(i) || (b[i - 1] == b'b' && !ident(i - 1)) => {
                    let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                    if b.get(i + 1 + hashes) != Some(&b'"') {
                        i += 1;
                        continue;
                    }
                    let start = i + 2 + hashes;
                    let end_mark = format!("\"{}", "#".repeat(hashes));
                    let end = src[start..].find(&end_mark).map_or(b.len(), |e| start + e);
                    found.push((src[..i].to_string(), src[start..end].to_string()));
                    i = end + end_mark.len();
                }
                b'"' => {
                    let start = i + 1;
                    let mut k = start;
                    while k < b.len() && b[k] != b'"' {
                        k += if b[k] == b'\\' { 2 } else { 1 };
                    }
                    let end = k.min(b.len());
                    found.push((src[..i].to_string(), src[start..end].to_string()));
                    i = end + 1;
                }
                _ => i += 1,
            }
        }
        found
    }

    /// Each product-name literal of the suite's non-test source outside
    /// this file that [`kept_name_literal`] does not keep, as `file: literal`.
    fn product_name_literals(root: &std::path::Path) -> Vec<String> {
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
                if file == "brand.rs" {
                    continue;
                }
                let src = without_test_items(&std::fs::read_to_string(&path).unwrap());
                for (before, literal) in string_literals(&src) {
                    if literal.to_ascii_lowercase().contains("docxy")
                        && !kept_name_literal(&file, &literal, &before)
                    {
                        found.push(format!("{file}: {literal}"));
                    }
                }
            }
        }
        found.sort();
        found
    }

    /// The suite shows its name only through this module, so an integration
    /// plugin's name replaces it. The guard does not see a name
    /// built from parts: `concat!` or `format!` pieces, or char escapes.
    #[test]
    fn the_product_name_is_written_only_here() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let found = product_name_literals(&root);
        assert!(found.is_empty(), "take brand:: for these: {found:#?}");
    }

    /// The guard's scanner: a literal in code counts, one in a comment, a
    /// test item (a test field, variant or match arm among them) or a char
    /// literal does not, and only the identifiers and literals it names are
    /// kept.
    #[test]
    fn the_guard_finds_a_name_in_code_and_not_in_comments_or_tests() {
        let src = concat!(
            "// \"About docxy\" in a comment\n",
            "/* \"docxy\" in a block */\n",
            "const A: &str = \"Quit docxy\";\n",
            "const B: char = '\"';\n",
            "fn f<'a>(x: &'a str) -> &'a str { x }\n",
            "const C: &str = r#\"docxy \"raw\"\"#;\n",
            "const D: &str = \"a \\\" docxy\";\n",
            "fn g() { root.join(\"docxy\"); env(\"DOCXY_X\"); id(\"docxy-find\"); }\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    const T: &str = \"docxy in a test\";\n",
            "}\n",
            "const E: &str = \"After docxy\";\n",
            "const F: &str = \"docxy\";\n",
            "const G: &str = \"Docxy\";\n",
            "struct S {\n",
            "    #[cfg(test)]\n",
            "    t: &'static str,\n",
            "}\n",
            "const H: &str = \"After a test field docxy\";\n",
            "enum V {\n",
            "    #[cfg(test)]\n",
            "    T,\n",
            "}\n",
            "const I: &str = \"After a test variant docxy\";\n",
            "fn h(x: u8) -> &'static str {\n",
            "    match x {\n",
            "        #[cfg(test)]\n",
            "        0 => \"a test arm docxy\",\n",
            "        _ => \"After a test arm docxy\",\n",
            "    }\n",
            "}\n",
        );
        let named: Vec<String> = string_literals(&without_test_items(src))
            .into_iter()
            .filter(|(before, l)| {
                l.to_ascii_lowercase().contains("docxy") && !kept_name_literal("x.rs", l, before)
            })
            .map(|(_, l)| l)
            .collect();
        assert_eq!(
            named,
            [
                "Quit docxy",
                "docxy \"raw\"",
                "a \\\" docxy",
                "docxy-find",
                "After docxy",
                "docxy",
                "Docxy",
                "After a test field docxy",
                "After a test variant docxy",
                "After a test arm docxy"
            ]
        );
    }
}
