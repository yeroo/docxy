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
/// desktop it reports, and its state. None with no plugin configured.
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
/// `Theme::change`: the primary on `primary`, its hover and active shades and
/// `ring`, its text on `primary_foreground`, and on `selection` at the
/// theme's own opacity; the link on `link` and its hover and active shades.
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
mod tests {
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
}
