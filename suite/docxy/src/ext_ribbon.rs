//! An extension's ribbon in the suite (#1870): the [`ext::Ribbon`] a
//! surface describes, converted once into the ribbonspec model the
//! document's ribbon is drawn from ([`convert`]), and cached on the tab
//! ([`ExtTab`]) until the surface ran a command or said it changed
//! ([`ext::ribbon_changed`] bumps a generation every window compares).
//!
//! The model holds `&'static str`, so every string is interned
//! (`plugin_ui::intern`), and a command's act is [`Act::Ext`] with its id.
//! A command's icon is the extension's SVG, served to gpui's asset loader
//! as `icons/ext/<hash>.svg` ([`icon_asset`]) and drawn as a mask in the
//! ribbon's ink, as the bundled Fluent icons are.
//!
//! The suite's theme as an extension reads it is [`suite_theme`].

use crate::ext::{self, RibbonCommand, RibbonControl};
use crate::plugin_ui::{capped_label, intern};
use crate::{Act, Docxy, RibbonTab, Surface, rs};
use gpui::{App, Hsla, Window};
use gpui_component::ActiveTheme as _;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Deref;
use std::sync::{Arc, Mutex};

/// Command ids the suite's own ribbon code gives a meaning to: a control
/// using one is left out of an extension's ribbon.
const RESERVED_IDS: [&str; 2] = [crate::menu::QAT_UNDO_ID, crate::menu::QAT_REDO_ID];

/// The gallery id `render_control` draws as Word's Table Styles.
const RESERVED_GALLERY: &str = "tablestyles";

/// An extension's tab body (`Surface::Ext`): the surface, and the ribbon it
/// described when last read. Both travel with the tab when it is torn off.
pub(crate) struct ExtTab {
    surface: Box<dyn ext::TabSurface>,
    /// The generation it was read at and what it returned; `None` before
    /// the first read.
    cache: Option<(u64, Option<ExtRibbon>)>,
}

impl ExtTab {
    pub(crate) fn new(surface: Box<dyn ext::TabSurface>) -> Self {
        ExtTab {
            surface,
            cache: None,
        }
    }

    /// The ribbon read last, when the surface describes one.
    pub(crate) fn ribbon(&self) -> Option<&ExtRibbon> {
        self.cache.as_ref().and_then(|(_, r)| r.as_ref())
    }

    /// Reads the surface's ribbon again when the cached one is older than
    /// `generation` (or `force`). Whether it read.
    pub(crate) fn refresh(&mut self, generation: u64, force: bool, cx: &App) -> bool {
        if !self.needs_read(generation, force) {
            return false;
        }
        // A ribbon with nothing left to draw is none: the view draws its own.
        let ribbon = self
            .surface
            .ribbon(cx)
            .map(|r| convert(&r))
            .filter(|r| !r.tabs.is_empty());
        self.cache = Some((generation, ribbon));
        true
    }
}

impl ExtTab {
    /// Whether the cached ribbon is to be read again at `generation`.
    fn needs_read(&self, generation: u64, force: bool) -> bool {
        force || self.cache.as_ref().is_none_or(|(g, _)| *g != generation)
    }
}

impl Deref for ExtTab {
    type Target = dyn ext::TabSurface;
    fn deref(&self) -> &Self::Target {
        &*self.surface
    }
}

/// The enabled and checked state of one command id.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CmdState {
    enabled: bool,
    checked: bool,
}

/// An extension's ribbon as the suite draws it.
pub(crate) struct ExtRibbon {
    /// The tabs after File, each with at least one control.
    pub(crate) tabs: Vec<rs::Tab<Act>>,
    state: HashMap<&'static str, CmdState>,
    /// The drop-down buttons' ids: a KeyTip on one opens its menu.
    dropdowns: HashSet<&'static str>,
}

impl ExtRibbon {
    /// Whether `id` is a command of this ribbon and enabled.
    pub(crate) fn enabled(&self, id: &str) -> bool {
        self.state.get(id).is_some_and(|s| s.enabled)
    }

    /// Whether `id` is a command of this ribbon and checked.
    pub(crate) fn checked(&self, id: &str) -> bool {
        self.state.get(id).is_some_and(|s| s.checked)
    }

    /// Whether `id` names a drop-down button.
    pub(crate) fn is_dropdown(&self, id: &str) -> bool {
        self.dropdowns.contains(id)
    }

    /// The tab strip after File: each tab's name and KeyTip.
    pub(crate) fn tab_names(&self) -> Vec<(&'static str, &'static str)> {
        self.tabs.iter().map(|t| (t.name, t.key_tip)).collect()
    }
}

/// `r` in the ribbonspec model. A command with a reserved id (or none) and
/// a gallery named `tablestyles` are left out, then any group and tab left
/// empty; a column of small buttons holds three, the rest go on to the
/// next column. An id's state is the last of its commands'.
pub(crate) fn convert(r: &ext::Ribbon) -> ExtRibbon {
    let mut out = ExtRibbon {
        tabs: Vec::new(),
        state: HashMap::new(),
        dropdowns: HashSet::new(),
    };
    for tab in &r.tabs {
        let groups: Vec<rs::Group<Act>> = tab
            .groups
            .iter()
            .filter_map(|g| {
                let items: Vec<rs::Control<Act>> = g
                    .controls
                    .iter()
                    .flat_map(|c| control(c, &mut out))
                    .collect();
                (!items.is_empty()).then(|| rs::group(intern(&g.title), g.priority, items))
            })
            .collect();
        if !groups.is_empty() {
            out.tabs
                .push(rs::tab(intern(&tab.name), intern(&tab.key_tip), groups));
        }
    }
    out
}

/// Whether a command can be part of an extension's ribbon.
fn usable(c: &RibbonCommand) -> bool {
    !c.id.is_empty() && !RESERVED_IDS.contains(&c.id.as_str())
}

/// One control as the model's controls (a long column is several).
fn control(c: &RibbonControl, out: &mut ExtRibbon) -> Vec<rs::Control<Act>> {
    let mut cmd = |c: &RibbonCommand, small: bool| -> Option<rs::Cmd<Act>> {
        usable(c).then(|| command(c, small, out))
    };
    match c {
        RibbonControl::Large(c) => cmd(c, false).map(rs::Control::Large).into_iter().collect(),
        RibbonControl::Toggle(c) => cmd(c, true).map(rs::Control::Toggle).into_iter().collect(),
        RibbonControl::Small(cs) => {
            let cmds: Vec<rs::Cmd<Act>> = cs.iter().filter_map(|c| cmd(c, true)).collect();
            cmds.chunks(rs::MAX_COLUMN_ROWS)
                .map(|chunk| rs::Control::Column(chunk.to_vec()))
                .collect()
        }
        RibbonControl::Split(s) => {
            let Some(primary) = cmd(&s.primary, false) else {
                return Vec::new();
            };
            let menu = s.menu.iter().filter_map(|c| cmd(c, false)).collect();
            vec![rs::Control::Split { primary, menu }]
        }
        RibbonControl::Dropdown(d) => {
            let Some(button) = cmd(&d.command, false) else {
                return Vec::new();
            };
            let items = d.items.iter().filter_map(|c| cmd(c, false)).collect();
            out.dropdowns.insert(button.id);
            vec![rs::Control::Dropdown { cmd: button, items }]
        }
        RibbonControl::Gallery(g) => {
            if g.id.is_empty() || g.id == RESERVED_GALLERY {
                return Vec::new();
            }
            let items: Vec<rs::GalleryItem<Act>> = g
                .items
                .iter()
                .filter_map(|c| cmd(c, false))
                .map(|c| rs::GalleryItem {
                    label: c.label,
                    preview: "",
                    section: "",
                    act: c.act,
                })
                .collect();
            if items.is_empty() {
                return Vec::new();
            }
            let tip = if g.tip.is_empty() { &g.id } else { &g.tip };
            vec![rs::Control::Gallery(rs::Gallery {
                id: intern(&g.id),
                tip: rs::ScreenTip {
                    title: intern(tip),
                    ..rs::ScreenTip::default()
                },
                key_tip: intern(&g.key_tip),
                items,
            })]
        }
    }
}

/// One command in the model, its state recorded. A small button's label is
/// cut as a plugin's is ([`capped_label`]), its full text the tip then.
fn command(c: &RibbonCommand, small: bool, out: &mut ExtRibbon) -> rs::Cmd<Act> {
    let id = intern(&c.id);
    let label = if small {
        capped_label(&c.label)
    } else {
        c.label.clone()
    };
    let title = if label != c.label || c.tip.is_empty() {
        &c.label
    } else {
        &c.tip
    };
    let icon = c.icon.as_ref().map_or("", register_icon);
    out.state.insert(
        id,
        CmdState {
            enabled: c.enabled,
            checked: c.checked,
        },
    );
    rs::cmd(id, icon, intern(&label), Act::Ext(id))
        .tip(intern(title), "", "")
        .key(intern(&c.key_tip))
}

/// The extensions' icons by content hash, kept for the life of the process.
static ICONS: Mutex<Option<HashMap<u64, Arc<[u8]>>>> = Mutex::new(None);

/// The asset directory an extension's icon is served from, under `icons/`.
const ICON_DIR: &str = "ext/";

/// `icon`'s name in the model (`ext/<hash>`), its bytes kept for
/// [`icon_asset`].
fn register_icon(icon: &ext::RibbonIcon) -> &'static str {
    let mut h = DefaultHasher::new();
    icon.source().hash(&mut h);
    let key = h.finish();
    let mut icons = ICONS.lock().unwrap_or_else(|e| e.into_inner());
    icons
        .get_or_insert_with(HashMap::new)
        .entry(key)
        .or_insert_with(|| Arc::from(icon.source()));
    intern(&format!("{ICON_DIR}{key:016x}"))
}

/// The SVG an `icons/ext/<hash>.svg` asset path names, when an extension
/// registered it.
pub(crate) fn icon_asset(path: &str) -> Option<Arc<[u8]>> {
    let hex = path
        .strip_prefix("icons/")?
        .strip_prefix(ICON_DIR)?
        .strip_suffix(".svg")?;
    let key = u64::from_str_radix(hex, 16).ok()?;
    let icons = ICONS.lock().unwrap_or_else(|e| e.into_inner());
    icons.as_ref()?.get(&key).cloned()
}

/// The suite's theme now, as [`ext::suite_theme`] gives it: the colours
/// `render` draws the window with, and the Mail tabs' accent.
pub(crate) fn suite_theme(cx: &App) -> ext::SuiteTheme {
    let t = cx.theme();
    theme_of(t, t.mode.is_dark())
}

/// [`suite_theme`] of the theme colours `t`, dark or light.
fn theme_of(t: &gpui_component::ThemeColor, dark: bool) -> ext::SuiteTheme {
    ext::SuiteTheme {
        dark,
        background: t.background,
        panel: t.secondary,
        border: t.border,
        accent: Hsla::from(gpui::rgb(crate::accent::for_kind(
            Some(crate::Kind::Look),
            dark,
        ))),
        text: t.foreground,
        muted_text: t.muted_foreground,
        selection: t.selection,
    }
}

/// The ribbon tab an extension's tab shows: `tab` while it is one of its
/// `n` tabs, else its first.
pub(crate) fn valid_tab(tab: RibbonTab, n: usize) -> RibbonTab {
    match tab {
        RibbonTab::Ext(i) if usize::from(i) < n => tab,
        _ => RibbonTab::Ext(0),
    }
}

/// The index of the tab that was `old[i]` among `new`'s: the same index
/// while it holds the same name, else the tab of that name, else the first.
pub(crate) fn reselect(old: &[&str], new: &[&str], i: usize) -> usize {
    let Some(name) = old.get(i) else {
        return 0;
    };
    if new.get(i) == Some(name) {
        return i;
    }
    new.iter().position(|n| n == name).unwrap_or(0)
}

impl Docxy {
    /// The active tab's extension ribbon, when it describes one.
    pub(crate) fn active_ext_ribbon(&self) -> Option<&ExtRibbon> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Ext(e)) => e.ribbon(),
            _ => None,
        }
    }

    /// The ribbon's tab strip for the active tab: File, then the
    /// extension's tabs on an extension's tab with a ribbon, else the
    /// kind's tabs.
    pub(crate) fn ribbon_tab_names(&self) -> Vec<(Option<RibbonTab>, &'static str, &'static str)> {
        match self.active_ext_ribbon() {
            Some(r) => {
                let mut names = vec![crate::ribbon_tab_set(self.ribbon_kind())[0]];
                names.extend(
                    r.tab_names()
                        .into_iter()
                        .enumerate()
                        .map(|(i, (name, key))| (Some(RibbonTab::Ext(tab_index(i))), name, key)),
                );
                names
            }
            None => crate::ribbon_tab_set(self.ribbon_kind()).to_vec(),
        }
    }

    /// A ribbon tab's name, an extension's tab's included.
    pub(crate) fn ribbon_tab_label(&self, tab: RibbonTab) -> &'static str {
        match (tab, self.active_ext_ribbon()) {
            (RibbonTab::Ext(i), Some(r)) => r.tabs.get(usize::from(i)).map_or("", |t| t.name),
            _ => crate::ribbon_tab_name(tab),
        }
    }

    /// Reads again every extension tab's ribbon read before the last
    /// [`ext::ribbon_changed`] (or never read), and `force_active`'s too.
    /// The active tab keeps showing the ribbon tab of the same name.
    pub(crate) fn refresh_ext_ribbons(&mut self, force_active: bool, cx: &App) {
        let generation = ext::ribbon_generation(cx);
        let active = self.active;
        let mut reselected = None;
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let Surface::Ext(e) = &mut t.surface else {
                continue;
            };
            let force = force_active && i == active;
            if !e.needs_read(generation, force) {
                continue;
            }
            let names = |e: &ExtTab| -> Vec<&'static str> {
                e.ribbon()
                    .map(|r| r.tab_names().into_iter().map(|(n, _)| n).collect())
                    .unwrap_or_default()
            };
            let old = names(e);
            e.refresh(generation, force, cx);
            if let (true, RibbonTab::Ext(k)) = (i == active, self.ribbon_tab) {
                reselected = Some(tab_index(reselect(&old, &names(e), usize::from(k))));
            }
        }
        if let Some(k) = reselected {
            self.ribbon_tab = RibbonTab::Ext(k);
        }
        // The active tab shows one of its tabs before any frame does.
        if let Some(r) = self.active_ext_ribbon() {
            self.ribbon_tab = valid_tab(self.ribbon_tab, r.tabs.len());
        }
    }

    /// Tells every extension tab of this window the theme now.
    pub(crate) fn tell_ext_theme(&self, cx: &mut App) {
        let theme = suite_theme(cx);
        for t in &self.tabs {
            if let Surface::Ext(e) = &t.surface {
                e.theme_changed(&theme, cx);
            }
        }
    }

    /// `Act::Ext(id)`: the active surface's command, unless it is disabled
    /// (the one guard for a click, a KeyTip, a menu or gallery item and the
    /// harness's `ribbon-click`). A drop-down's own id opens its menu, as a
    /// built-in drop-down's KeyTip does. The ribbon is read again after.
    pub(crate) fn ext_dispatch(
        &mut self,
        id: &'static str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(r) = self.active_ext_ribbon() else {
            return;
        };
        if !r.enabled(id) {
            self.set_status("That command is not available now");
            cx.notify();
            return;
        }
        if r.is_dropdown(id) {
            let at = crate::split_menu_anchor(&self.probes.borrow(), id)
                .unwrap_or_else(|| gpui::point(gpui::px(12.), gpui::px(140.)));
            if let Err(e) = self.open_split_menu(id, at, cx) {
                self.set_status(e);
            }
            self.focus_ext_surface(window, cx);
            cx.notify();
            return;
        }
        if let Some(Surface::Ext(e)) = self.tabs.get(self.active).map(|t| &t.surface) {
            e.ribbon_action(id, window, cx);
        }
        self.refresh_ext_ribbons(true, cx);
        cx.notify();
    }
}

/// A tab index as `RibbonTab::Ext` holds it (an extension with more than
/// 255 tabs shows its first 256).
fn tab_index(i: usize) -> u8 {
    u8::try_from(i).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ext::{
        Ribbon, RibbonDropdown, RibbonGallery, RibbonGroup, RibbonIcon, RibbonSplit,
        RibbonTab as ExtTabDef,
    };

    const SVG: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'><rect width='8' height='8'/></svg>";

    fn c(id: &str) -> RibbonCommand {
        RibbonCommand::new(id, id)
    }

    fn sample() -> Ribbon {
        Ribbon::new(vec![
            ExtTabDef::new(
                "Message",
                "M",
                vec![
                    RibbonGroup::new(
                        "Respond",
                        vec![
                            RibbonControl::Large(
                                c("fx.reply")
                                    .icon(RibbonIcon::svg(SVG))
                                    .key_tip("R")
                                    .tip("Reply to the sender"),
                            ),
                            RibbonControl::Small(vec![c("a"), c("b"), c("c"), c("d"), c("e")]),
                            RibbonControl::Toggle(c("fx.flag").checked(true)),
                            RibbonControl::Split(RibbonSplit::new(
                                c("fx.move"),
                                vec![c("fx.move.inbox"), c("fx.move.archive")],
                            )),
                            RibbonControl::Large(c("fx.archive").enabled(false)),
                        ],
                    )
                    .priority(120),
                ],
            ),
            ExtTabDef::new(
                "View",
                "V",
                vec![RibbonGroup::new(
                    "Layout",
                    vec![
                        RibbonControl::Dropdown(RibbonDropdown::new(
                            c("fx.sort"),
                            vec![c("fx.sort.date"), c("fx.sort.from")],
                        )),
                        RibbonControl::Gallery(
                            RibbonGallery::new(
                                "fx.density",
                                vec![c("fx.d1"), c("fx.d2").checked(true), c("fx.d3")],
                            )
                            .key_tip("D"),
                        ),
                    ],
                )],
            ),
        ])
    }

    #[test]
    fn every_control_kind_converts_to_the_model() {
        let r = convert(&sample());
        assert_eq!(r.tab_names(), vec![("Message", "M"), ("View", "V")]);
        let g = &r.tabs[0].groups[0];
        assert_eq!((g.title, g.priority), ("Respond", 120));
        // Large, two columns (3 + 2), Toggle, Split, Large.
        assert_eq!(g.items.len(), 6);
        let rs::Control::Large(reply) = &g.items[0] else {
            panic!("a large button");
        };
        assert_eq!(
            (reply.id, reply.label, reply.key_tip),
            ("fx.reply", "fx.reply", "R")
        );
        assert_eq!(reply.tip.title, "Reply to the sender");
        assert_eq!(reply.act, Act::Ext("fx.reply"));
        assert!(reply.icon.0.starts_with("ext/"), "{}", reply.icon.0);
        let cols: Vec<usize> = g.items[1..3]
            .iter()
            .map(|c| match c {
                rs::Control::Column(cs) => cs.len(),
                _ => panic!("a column"),
            })
            .collect();
        assert_eq!(cols, [3, 2]);
        assert!(matches!(&g.items[3], rs::Control::Toggle(t) if t.id == "fx.flag"));
        let rs::Control::Split { primary, menu } = &g.items[4] else {
            panic!("a split");
        };
        assert_eq!(primary.id, "fx.move");
        assert_eq!(
            menu.iter().map(|m| m.id).collect::<Vec<_>>(),
            ["fx.move.inbox", "fx.move.archive"]
        );
        let v = &r.tabs[1].groups[0];
        let rs::Control::Dropdown { cmd, items } = &v.items[0] else {
            panic!("a dropdown");
        };
        assert_eq!((cmd.id, items.len()), ("fx.sort", 2));
        assert!(r.is_dropdown("fx.sort") && !r.is_dropdown("fx.move"));
        let rs::Control::Gallery(gal) = &v.items[1] else {
            panic!("a gallery");
        };
        assert_eq!(
            (gal.id, gal.key_tip, gal.items.len()),
            ("fx.density", "D", 3)
        );
        assert_eq!(gal.items[1].act, Act::Ext("fx.d2"));
        // A gallery of extension commands is drawn as bare tiles.
        assert!(crate::plugin_ui::is_swatch_gallery(gal));
        // No row stack is over three.
        assert!(r.tabs.iter().all(|t| t.row_violations().is_empty()));
    }

    #[test]
    fn the_state_map_holds_enabled_and_checked() {
        let r = convert(&sample());
        assert!(r.enabled("fx.reply") && !r.checked("fx.reply"));
        assert!(r.checked("fx.flag"));
        assert!(!r.enabled("fx.archive"));
        assert!(r.checked("fx.d2") && !r.checked("fx.d1"));
        // An id the ribbon does not hold is neither.
        assert!(!r.enabled("nope") && !r.checked("nope"));
    }

    #[test]
    fn an_icon_is_served_at_its_asset_path() {
        let r = convert(&sample());
        let rs::Control::Large(reply) = &r.tabs[0].groups[0].items[0] else {
            panic!("a large button");
        };
        let path = format!("icons/{}.svg", reply.icon.0);
        assert_eq!(icon_asset(&path).as_deref(), Some(SVG));
        // The same bytes are the same name.
        assert_eq!(register_icon(&RibbonIcon::svg(SVG)), reply.icon.0);
        assert!(icon_asset("icons/ext/zz.svg").is_none());
        assert!(icon_asset("icons/bold.svg").is_none());
        // A command with no icon has an empty one (drawn with its label).
        let rs::Control::Toggle(flag) = &r.tabs[0].groups[0].items[3] else {
            panic!("a toggle");
        };
        assert_eq!(flag.icon.0, "");
    }

    #[test]
    fn reserved_ids_and_empty_groups_and_tabs_are_left_out() {
        let r = convert(&Ribbon::new(vec![
            ExtTabDef::new(
                "Kept",
                "K",
                vec![
                    RibbonGroup::new(
                        "One",
                        vec![
                            RibbonControl::Large(c(crate::menu::QAT_UNDO_ID)),
                            RibbonControl::Small(vec![c("qat-redo"), c("ok"), c("")]),
                            RibbonControl::Split(RibbonSplit::new(
                                c("ok.split"),
                                vec![c("qat-undo"), c("ok.item")],
                            )),
                        ],
                    ),
                    RibbonGroup::new(
                        "Gone",
                        vec![RibbonControl::Gallery(RibbonGallery::new(
                            "tablestyles",
                            vec![c("t1")],
                        ))],
                    ),
                ],
            ),
            ExtTabDef::new("Empty", "E", vec![RibbonGroup::new("Nothing", vec![])]),
        ]));
        assert_eq!(r.tab_names(), vec![("Kept", "K")]);
        let groups = &r.tabs[0].groups;
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].items.len(), 2);
        assert!(
            matches!(&groups[0].items[0], rs::Control::Column(cs) if cs.len() == 1 && cs[0].id == "ok")
        );
        let rs::Control::Split { menu, .. } = &groups[0].items[1] else {
            panic!("a split");
        };
        assert_eq!(menu.iter().map(|m| m.id).collect::<Vec<_>>(), ["ok.item"]);
        assert!(!r.enabled("qat-undo") && !r.enabled("t1"));
    }

    #[test]
    fn a_small_label_is_capped_with_its_full_text_the_tip() {
        let long = "A very long command label for a small button";
        let r = convert(&Ribbon::new(vec![ExtTabDef::new(
            "T",
            "",
            vec![RibbonGroup::new(
                "G",
                vec![
                    RibbonControl::Small(vec![RibbonCommand::new("s", long).tip("ignored")]),
                    RibbonControl::Large(RibbonCommand::new("l", long)),
                ],
            )],
        )]));
        let items = &r.tabs[0].groups[0].items;
        let rs::Control::Column(cs) = &items[0] else {
            panic!("a column");
        };
        assert_eq!(
            cs[0].label.chars().count(),
            crate::plugin_ui::CONTRIB_LABEL_MAX
        );
        assert!(cs[0].label.ends_with('\u{2026}'));
        assert_eq!(cs[0].tip.title, long);
        // A large button wraps its label instead: kept whole.
        let rs::Control::Large(l) = &items[1] else {
            panic!("a large button");
        };
        assert_eq!((l.label, l.tip.title), (long, long));
    }

    #[test]
    fn an_ext_tab_index_is_kept_valid() {
        assert_eq!(valid_tab(RibbonTab::Ext(1), 2), RibbonTab::Ext(1));
        assert_eq!(valid_tab(RibbonTab::Ext(2), 2), RibbonTab::Ext(0));
        assert_eq!(valid_tab(RibbonTab::Home, 2), RibbonTab::Ext(0));
    }

    #[test]
    fn a_refreshed_ribbon_keeps_the_tab_of_the_same_name() {
        let old = ["Message", "View"];
        assert_eq!(reselect(&old, &old, 1), 1);
        // A tab inserted before it: the same name, one further on.
        assert_eq!(reselect(&old, &["Compose", "Message", "View"], 1), 2);
        // Its tab gone: the first.
        assert_eq!(reselect(&old, &["Message"], 1), 0);
        assert_eq!(reselect(&old, &["Message", "Other"], 1), 0);
        assert_eq!(reselect(&[], &old, 3), 0);
    }

    #[test]
    fn the_tab_keytips_pick_the_extension_tabs_and_file_keeps_f() {
        let r = convert(&Ribbon::new(vec![
            ExtTabDef::new("Message", "M", sample().tabs[0].groups.clone()),
            ExtTabDef::new("Folder", "F", sample().tabs[0].groups.clone()),
            ExtTabDef::new("View", "VW", sample().tabs[1].groups.clone()),
        ]));
        let pick = |typed: &str| crate::ext_tab_keytip_pick(&r, typed);
        assert_eq!(pick("m"), crate::TabPick::Tab(RibbonTab::Ext(0)));
        // File is first: an extension tab's `F` never takes it.
        assert_eq!(pick("F"), crate::TabPick::Backstage);
        assert_eq!(pick("V"), crate::TabPick::Wait);
        assert_eq!(pick("VW"), crate::TabPick::Tab(RibbonTab::Ext(2)));
        assert_eq!(pick("Q"), crate::TabPick::Off);
    }

    /// A surface the cache test never draws.
    struct Stub;

    impl ext::TabSurface for Stub {
        fn view(&self) -> gpui::AnyView {
            unreachable!("not drawn")
        }
        fn focus_handle(&self, _cx: &App) -> gpui::FocusHandle {
            unreachable!("not focused")
        }
    }

    #[test]
    fn a_cached_ribbon_is_read_again_only_after_a_change() {
        let mut tab = ExtTab::new(Box::new(Stub));
        assert!(tab.needs_read(0, false), "never read");
        tab.cache = Some((3, Some(convert(&sample()))));
        assert!(!tab.needs_read(3, false), "fresh");
        assert!(tab.needs_read(4, false), "ext::ribbon_changed bumped it");
        assert!(tab.needs_read(3, true), "forced after an action");
        assert!(tab.ribbon().is_some_and(|r| r.checked("fx.flag")));
        // A surface that describes no ribbon is cached as none.
        tab.cache = Some((3, None));
        assert!(tab.ribbon().is_none() && !tab.needs_read(3, false));
    }

    #[test]
    fn the_suite_theme_differs_light_and_dark() {
        use gpui_component::ThemeColor;
        let light = theme_of(&ThemeColor::light(), false);
        let dark = theme_of(&ThemeColor::dark(), true);
        assert!(!light.dark && dark.dark);
        assert_ne!(light.background, dark.background);
        assert_ne!(light.text, dark.text);
        assert_eq!(light.background, ThemeColor::light().background);
        assert_eq!(dark.panel, ThemeColor::dark().secondary);
        let accent = |dark| {
            Hsla::from(gpui::rgb(crate::accent::for_kind(
                Some(crate::Kind::Look),
                dark,
            )))
        };
        assert_eq!((light.accent, dark.accent), (accent(false), accent(true)));
    }
}
