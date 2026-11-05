//! Font substitution before gpui (#1900): every family a document, a plugin
//! or the theme names is resolved once per process to one gpui can really
//! load, and only the resolved family is ever handed to gpui.
//!
//! Why: gpui caches a failed family lookup but turns the cached failure into
//! a new `anyhow::Error` on every `font_id` call, and `resolve_font` then
//! walks its own fallback stack (`.ZedMono`, `.ZedSans`, Helvetica, Segoe
//! UI, ...), most of which fail on Linux too, for every run of every layout
//! pass. With `RUST_BACKTRACE=1` each of those errors captures a backtrace.
//! A document in Calibri on a machine without Calibri, or the theme's
//! `.SystemUIFont` (gpui maps it to IBM Plex Sans on Linux, which the suite
//! does not embed), pinned the UI thread.
//!
//! Order for a document family ([`family`]): the family itself (any case,
//! drawn in its installed spelling), then `docxcore::fonts::substitutes`
//! (metric-compatible first, shared with PDF export), then a generic serif or
//! mono class, else `None`: draw in the inherited (theme) font. The theme's
//! own families go through [`ui_family`] / [`ui_mono_family`].

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use gpui::{App, SharedString, font};

/// The UI sans families tried for an unloadable theme font: gpui's own
/// fallback order, so the face drawn is the one `resolve_font` lands on
/// today, without failing first.
const UI_SANS: &[&str] = &[
    "Ubuntu",
    "Adwaita Sans",
    "Cantarell",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Arial",
];

/// A monospaced family's generic substitutes.
const MONO: &[&str] = &[
    "Liberation Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Cousine",
    "Menlo",
    "Consolas",
    "Courier New",
];

/// A serif family's generic substitutes.
const SERIF: &[&str] = &[
    "Liberation Serif",
    "DejaVu Serif",
    "Noto Serif",
    "Tinos",
    "Times New Roman",
    "Georgia",
];

/// The generic class a family name suggests (a stand-in for the platform's
/// own matching, such as fontconfig's `FcFontMatch`): mono, serif or none
/// (sans, drawn in the theme font).
fn generic_class(lower: &str) -> &'static [&'static str] {
    const MONO_HINTS: &[&str] = &["mono", "courier", "consol", "code"];
    const SERIF_HINTS: &[&str] = &["times", "roman", "georgia", "garamond", "book", "cambria"];
    if MONO_HINTS.iter().any(|h| lower.contains(h)) {
        MONO
    } else if (lower.contains("serif") && !lower.contains("sans"))
        || SERIF_HINTS.iter().any(|h| lower.contains(h))
    {
        SERIF
    } else {
        &[]
    }
}

type Probe = Box<dyn Fn(&str) -> bool + Send>;

/// Resolves family names against the installed fonts, each candidate probed
/// at most once.
pub(crate) struct Resolver {
    /// Installed family names, lower case → their own spelling. gpui's list
    /// also holds its fallback stack and `.SystemUIFont` whether installed or
    /// not, hence the probe. It holds one name per family (Linux: each face's
    /// first family name; Windows: one localized name), while gpui loads more
    /// ("Calibri Light", "Segoe UI Semibold", localized names), so a requested
    /// family not listed is probed too ([`Resolver::requested`]).
    installed: HashMap<String, SharedString>,
    /// Whether gpui really loads a family (it may make one failed lookup).
    probe: Probe,
    /// Probe results of listed families, by lower-case family.
    probed: HashMap<String, bool>,
    /// Probe results of requested families not listed, by the trimmed name
    /// as asked (Linux matches family names case-sensitively).
    probed_unlisted: HashMap<String, bool>,
    /// Resolved document families, by the exact name asked for.
    resolved: HashMap<String, Option<SharedString>>,
    /// Resolved theme families (sans and mono), by the exact name asked for.
    ui: HashMap<(bool, String), SharedString>,
    /// How many times `probe` ran.
    probes: usize,
}

impl Resolver {
    pub(crate) fn new(names: Vec<String>, probe: impl Fn(&str) -> bool + Send + 'static) -> Self {
        let installed = names
            .into_iter()
            .map(|n| (n.to_lowercase(), SharedString::from(n)))
            .collect();
        Resolver {
            installed,
            probe: Box::new(probe),
            probed: HashMap::new(),
            probed_unlisted: HashMap::new(),
            resolved: HashMap::new(),
            ui: HashMap::new(),
            probes: 0,
        }
    }

    /// `name`'s installed spelling when gpui loads it; a candidate not
    /// listed never reaches gpui.
    fn available(&mut self, name: &str) -> Option<SharedString> {
        let lower = name.trim().to_lowercase();
        let canonical = self.installed.get(&lower)?.clone();
        let ok = match self.probed.get(&lower) {
            Some(&ok) => ok,
            None => {
                self.probes += 1;
                let ok = (self.probe)(&canonical);
                self.probed.insert(lower, ok);
                ok
            }
        };
        ok.then_some(canonical)
    }

    fn first_available(&mut self, names: &[&str]) -> Option<SharedString> {
        names.iter().find_map(|n| self.available(n))
    }

    /// The family asked for itself, when gpui loads it: in its listed
    /// spelling, or as asked when the list does not hold it (probed once per
    /// name). Substitutes and generic candidates keep the list's gate.
    fn requested(&mut self, name: &str) -> Option<SharedString> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if self.installed.contains_key(&name.to_lowercase()) {
            return self.available(name);
        }
        let ok = match self.probed_unlisted.get(name) {
            Some(&ok) => ok,
            None => {
                self.probes += 1;
                let ok = (self.probe)(name);
                self.probed_unlisted.insert(name.to_string(), ok);
                ok
            }
        };
        ok.then(|| SharedString::from(name.to_string()))
    }

    /// The family to draw `family` in, or `None` for the inherited font.
    pub(crate) fn resolve(&mut self, family: &str) -> Option<SharedString> {
        if let Some(hit) = self.resolved.get(family) {
            return hit.clone();
        }
        let found = if family.trim().is_empty() {
            None
        } else {
            let lower = family.trim().to_lowercase();
            self.requested(family)
                .or_else(|| self.first_available(docxcore::fonts::substitutes(family)))
                .or_else(|| self.first_available(generic_class(&lower)))
        };
        self.resolved.insert(family.to_string(), found.clone());
        found
    }

    /// The theme's sans family when gpui loads it, else the first installed
    /// [`UI_SANS`], else `theme` unchanged.
    pub(crate) fn ui_family(&mut self, theme: &str) -> SharedString {
        self.ui_resolve(theme, false)
    }

    /// The theme's mono family when gpui loads it, else its substitutes or
    /// the first installed [`MONO`], else `theme` unchanged.
    pub(crate) fn ui_mono_family(&mut self, theme: &str) -> SharedString {
        self.ui_resolve(theme, true)
    }

    fn ui_resolve(&mut self, theme: &str, mono: bool) -> SharedString {
        let key = (mono, theme.to_string());
        if let Some(hit) = self.ui.get(&key) {
            return hit.clone();
        }
        let found = self
            .requested(theme)
            .or_else(|| {
                if mono {
                    self.first_available(docxcore::fonts::substitutes(theme))
                        .or_else(|| self.first_available(MONO))
                } else {
                    self.first_available(UI_SANS)
                }
            })
            .unwrap_or_else(|| SharedString::from(theme.to_string()));
        self.ui.insert(key, found.clone());
        found
    }

    #[cfg(test)]
    fn probes(&self) -> usize {
        self.probes
    }
}

static RESOLVER: OnceLock<Mutex<Resolver>> = OnceLock::new();

/// Builds the process-wide resolver from `cx`'s fonts (once; later calls do
/// nothing). It snapshots the installed names, so it runs after any
/// `add_fonts`: fonts added later are not seen.
pub(crate) fn init(cx: &App) {
    RESOLVER.get_or_init(|| {
        let ts = cx.text_system().clone();
        let names = ts.all_font_names();
        // The id `resolve_font` gives a family no one has: the first working
        // fallback's. Asked once, on the first probe.
        let missing = OnceLock::new();
        Mutex::new(Resolver::new(names, move |name| {
            // gpui's `font_id` is private: `resolve_font` hands back a
            // fallback's id for a family that fails. Every platform loads a
            // family into fresh ids, so a family loaded is one whose id names
            // it, or one whose id is not the fallback's.
            let id = ts.resolve_font(&font(SharedString::from(name.to_string())));
            let named = ts
                .get_font_for_id(id)
                .is_some_and(|f| f.family.eq_ignore_ascii_case(name));
            if named {
                // Not asking for the sentinel spares a walk of gpui's fallback
                // stack, which panics when none of it loads.
                return true;
            }
            let fallback =
                *missing.get_or_init(|| ts.resolve_font(&font("\u{1}docxy no such font")));
            id != fallback
        }))
    });
}

fn with<T>(f: impl FnOnce(&mut Resolver) -> T) -> Option<T> {
    let lock = RESOLVER.get()?;
    let mut resolver = lock.lock().unwrap_or_else(PoisonError::into_inner);
    Some(f(&mut resolver))
}

/// The family to draw a document's (or plugin's) `name` in; `None`: the
/// inherited font. Before [`init`] (unit tests without an app) `name` as it is.
pub(crate) fn family(name: &str) -> Option<SharedString> {
    with(|r| r.resolve(name)).unwrap_or_else(|| Some(SharedString::from(name.to_string())))
}

/// [`Resolver::ui_family`] for the theme's sans family `theme`; before
/// [`init`], `theme` itself.
pub(crate) fn ui_family(theme: &SharedString) -> SharedString {
    with(|r| r.ui_family(theme)).unwrap_or_else(|| theme.clone())
}

/// [`Resolver::ui_mono_family`] for the theme's mono family `theme`; before
/// [`init`], `theme` itself.
pub(crate) fn ui_mono_family(theme: &SharedString) -> SharedString {
    with(|r| r.ui_mono_family(theme)).unwrap_or_else(|| theme.clone())
}

/// Points the global theme's sans and mono families at loadable ones (run
/// after `gpui_component::init` and after every `Theme::change`; cheap and
/// idempotent). macOS and Windows keep theirs.
pub(crate) fn apply_to_theme(cx: &mut App) {
    let theme = gpui_component::Theme::global(cx);
    let sans = ui_family(&theme.font_family);
    let mono = ui_mono_family(&theme.mono_font_family);
    if sans != theme.font_family || mono != theme.mono_font_family {
        let theme = gpui_component::Theme::global_mut(cx);
        theme.font_family = sans;
        theme.mono_font_family = mono;
    }
}

#[cfg(test)]
mod tests;
