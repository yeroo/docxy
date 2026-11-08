//! Extension surfaces: a build of the suite may link a crate that draws a
//! tab kind the public suite leaves as a placeholder (today: Mail). The
//! public `suite` binary passes [`Extensions::default`], so nothing here
//! changes it; the seam names no particular extension.
//!
//! An extension's surface is a gpui view the tab hosts as its body
//! (`Surface::Ext`). The tab owns it like any other surface, so View >
//! Window > New Window and tearing the tab off the strip carry the same view,
//! state and all, into the new window. The suite still draws the title bar,
//! the tab strip and the status line; the view draws everything below the
//! tab strip and takes the keys while its tab is the active one.
//!
//! A surface may describe its ribbon as data ([`TabSurface::ribbon`],
//! #1870): the suite then draws it under its own tab strip with the same
//! code, theme, fit, collapse, flyouts, menus and KeyTips as a document's,
//! and calls [`TabSurface::ribbon_action`] with a command's id. Without one
//! the view draws its own ribbon, as before. [`suite_theme`] reads the
//! suite's colours; [`TabSurface::theme_changed`] says when they change.
//!
//! A surface may add pages to the suite's File backstage while its tab is
//! active ([`TabSurface::backstage_pages`], #1923), and its view may close
//! its own tab by dispatching [`CloseTab`].

use gpui::{AnyView, App, FocusHandle, Global, Hsla, Window, actions};
use std::rc::Rc;
use std::sync::Arc;

/// A view of a document an extension's view may embed (#1873): the suite's
/// own document renderer in web layout, with selection, copy, zoom,
/// scrolling and hyperlinks handed to the extension; read-only unless made
/// editable ([`DocView::set_editable`], #1947), when it types and edits and
/// takes the host ribbon's [`DocFormat`] commands. See [`DocView::new`].
pub use crate::doc_view::{CaretFormat, DocFormat, DocView, LinkClick};

actions!(
    docxy_ext,
    [
        /// Opens the suite's File backstage (New, Open, Recent, Settings).
        /// An extension's own File command dispatches it from its view.
        ShowBackstage,
        /// Closes the extension tab that is active (#1923), as the tab
        /// strip's close button does; File closes too when it is up. Dispatch
        /// it with `window.dispatch_action(Box::new(ext::CloseTab), cx)` from
        /// the view, from a [`BackstagePage`]'s view or from
        /// [`TabSurface::ribbon_action`], in response to the user's own input:
        /// it closes whichever extension tab is active when it runs, so a late
        /// dispatch (from a spawned task, after the user moved to another tab)
        /// closes that one. On a tab that is not an extension's it does
        /// nothing.
        CloseTab
    ]
);

/// A tab body an extension draws.
pub trait TabSurface: 'static {
    /// The view drawn as the tab's body.
    fn view(&self) -> AnyView;
    /// What the suite focuses when the tab becomes active, so the view's
    /// own key handling sees the keys first. While the suite's ribbon
    /// (#1870) takes the keys (its KeyTips, a menu, a flyout), it takes them
    /// from whatever in the view had them and gives them back to that, not
    /// to this, when they close.
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    /// The tab's title (the folder shown, say). `None` keeps the suite's.
    fn title(&self, _cx: &App) -> Option<String> {
        None
    }
    /// The suite's theme turned light or dark (View > Theme, Auto following
    /// the OS). The view may follow it.
    fn set_dark(&self, _dark: bool, _cx: &mut App) {}
    /// The ribbon the suite draws for this tab (#1870), or `None` (the
    /// default) for a view that draws its own. Read when the surface is
    /// attached, after each [`TabSurface::ribbon_action`] and after
    /// [`ribbon_changed`], never per frame: call [`ribbon_changed`] when
    /// what it returns would change (a toggle's state, a command enabled).
    ///
    /// Every distinct string in it (ids, labels, tips, KeyTips) is kept for
    /// the life of the process, as the suite's ribbon model holds
    /// `&'static str`: use stable ids, and keep counters out of them. The
    /// ids `qat-undo` and `qat-redo`, and the gallery id `tablestyles`, are
    /// the suite's: a control using one is left out. File's KeyTip is `F`,
    /// so a tab's KeyTip starting with `F` (any case) is dropped: the tab
    /// stays, with no KeyTip. While the KeyTips, a ribbon menu or a flyout are up the suite
    /// takes the keyboard from the view; let Alt and F10 bubble from the
    /// view's own key handling so the KeyTips can come up. A ribbon with no
    /// control left to draw is taken as `None`.
    fn ribbon(&self, _cx: &App) -> Option<Ribbon> {
        None
    }
    /// A command of [`TabSurface::ribbon`] ran: a click, its KeyTip, a menu
    /// or gallery item. `id` is the [`RibbonCommand`]'s. Never called for a
    /// disabled command. The suite reads the ribbon again afterwards.
    fn ribbon_action(&self, _id: &str, _window: &mut Window, _cx: &mut App) {}
    /// The suite's theme changed (and once when the surface is attached):
    /// its colours, as [`suite_theme`] reads them. [`TabSurface::set_dark`]
    /// is still called too.
    fn theme_changed(&self, _theme: &SuiteTheme, _cx: &mut App) {}
    /// The pages this tab adds to the suite's File backstage (#1923), in
    /// order; none by default. Read any number of times while the tab is
    /// active (each time File is drawn, more than once a frame, and on each
    /// key while File is up), so what it returns may change at any time with
    /// no call to the suite. Keep it cheap and free of side effects, and
    /// return the same views each time (cheap [`AnyView`] clones of entities
    /// the surface keeps), not new entities, or a page loses its state on
    /// every frame.
    ///
    /// [`BackstageGroup::Top`] entries go right after the rail's Back, ahead
    /// of the suite's own items, in this order; [`BackstageGroup::Bottom`]
    /// entries go after the suite's Account and before Close. A page with an
    /// empty id, or an id an earlier page already uses, is left out. Its view
    /// fills File's content column and is not scrolled by the suite: it
    /// scrolls itself, as a tab body does. Keys it does not stop bubble to
    /// File as on the suite's own pages.
    fn backstage_pages(&self, _cx: &App) -> Vec<BackstagePage> {
        Vec::new()
    }
}

/// Where a [`BackstagePage`]'s rail entry goes (#1923).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackstageGroup {
    /// Right after Back, ahead of the suite's items (Outlook's Info).
    #[default]
    Top,
    /// After the suite's Account, before Close (Outlook's Options).
    Bottom,
}

/// A page an extension tab adds to the suite's File backstage (#1923): its
/// rail entry (`label`, an optional `icon` drawn as a mask in the rail's
/// ink, as ribbon icons are, and its `group`) and the `view` drawn as the
/// page's body while the entry is selected. `id` names it: stable, unique
/// among the tab's pages.
#[non_exhaustive]
#[derive(Clone)]
pub struct BackstagePage {
    pub id: String,
    pub label: String,
    pub icon: Option<RibbonIcon>,
    pub group: BackstageGroup,
    pub view: AnyView,
}

impl BackstagePage {
    /// A [`BackstageGroup::Top`] page with no icon.
    pub fn new(id: impl Into<String>, label: impl Into<String>, view: impl Into<AnyView>) -> Self {
        BackstagePage {
            id: id.into(),
            label: label.into(),
            icon: None,
            group: BackstageGroup::Top,
            view: view.into(),
        }
    }
    pub fn icon(mut self, icon: RibbonIcon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn group(mut self, group: BackstageGroup) -> Self {
        self.group = group;
        self
    }
}

/// An extension's ribbon (#1870): its tabs after the suite's File, left to
/// right.
#[non_exhaustive]
#[derive(Clone, Debug, Default)]
pub struct Ribbon {
    pub tabs: Vec<RibbonTab>,
}

impl Ribbon {
    pub fn new(tabs: Vec<RibbonTab>) -> Self {
        Ribbon { tabs }
    }
}

/// One ribbon tab: its name, its KeyTip (Alt, then this; empty for none;
/// one starting with `F` is dropped, as File's `F` would take it first) and
/// its groups. A tab with no control is not drawn.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct RibbonTab {
    pub name: String,
    pub key_tip: String,
    pub groups: Vec<RibbonGroup>,
}

impl RibbonTab {
    pub fn new(
        name: impl Into<String>,
        key_tip: impl Into<String>,
        groups: Vec<RibbonGroup>,
    ) -> Self {
        RibbonTab {
            name: name.into(),
            key_tip: key_tip.into(),
            groups,
        }
    }
}

/// A titled group of controls. As the window narrows, groups of the lowest
/// `priority` drop their labels and then collapse into a button whose
/// flyout holds the group, as the suite's own do (default 50; the suite's
/// own run 0 to 200).
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct RibbonGroup {
    pub title: String,
    pub priority: u8,
    pub controls: Vec<RibbonControl>,
}

impl RibbonGroup {
    pub fn new(title: impl Into<String>, controls: Vec<RibbonControl>) -> Self {
        RibbonGroup {
            title: title.into(),
            priority: 50,
            controls,
        }
    }
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }
}

/// A control in a group.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum RibbonControl {
    /// A large button: the icon over the label.
    Large(RibbonCommand),
    /// Small buttons (icon and label) stacked three to a column; more go
    /// on to the next column.
    Small(Vec<RibbonCommand>),
    /// A small button drawn pressed while its command is `checked`.
    Toggle(RibbonCommand),
    /// A large button whose lower half opens a menu.
    Split(RibbonSplit),
    /// A large button that opens a menu.
    Dropdown(RibbonDropdown),
    /// A row of tiles, each a command; the `checked` one is drawn selected.
    Gallery(RibbonGallery),
}

/// [`RibbonControl::Split`]: `primary` runs on the upper half, `menu` lists
/// the commands the lower half opens.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct RibbonSplit {
    pub primary: RibbonCommand,
    pub menu: Vec<RibbonCommand>,
}

impl RibbonSplit {
    pub fn new(primary: RibbonCommand, menu: Vec<RibbonCommand>) -> Self {
        RibbonSplit { primary, menu }
    }
}

/// [`RibbonControl::Dropdown`]: `command` names the button (its id is never
/// run), `items` the menu it opens.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct RibbonDropdown {
    pub command: RibbonCommand,
    pub items: Vec<RibbonCommand>,
}

impl RibbonDropdown {
    pub fn new(command: RibbonCommand, items: Vec<RibbonCommand>) -> Self {
        RibbonDropdown { command, items }
    }
}

/// [`RibbonControl::Gallery`]: tiles showing each item's label. `key_tip`
/// opens it whole in a flyout.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct RibbonGallery {
    pub id: String,
    pub tip: String,
    pub key_tip: String,
    pub items: Vec<RibbonCommand>,
}

impl RibbonGallery {
    pub fn new(id: impl Into<String>, items: Vec<RibbonCommand>) -> Self {
        RibbonGallery {
            id: id.into(),
            tip: String::new(),
            key_tip: String::new(),
            items,
        }
    }
    pub fn tip(mut self, tip: impl Into<String>) -> Self {
        self.tip = tip.into();
        self
    }
    pub fn key_tip(mut self, key_tip: impl Into<String>) -> Self {
        self.key_tip = key_tip.into();
        self
    }
}

/// A command: what [`TabSurface::ribbon_action`] receives (`id`) and how it
/// is drawn. Enabled and not checked unless set. A small button's label
/// over 24 characters is cut with an ellipsis, its full text the tooltip.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct RibbonCommand {
    pub id: String,
    pub label: String,
    pub icon: Option<RibbonIcon>,
    /// The tooltip; empty shows the label.
    pub tip: String,
    /// Alt, the tab's KeyTip, then this; empty for none.
    pub key_tip: String,
    pub enabled: bool,
    pub checked: bool,
}

impl RibbonCommand {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        RibbonCommand {
            id: id.into(),
            label: label.into(),
            icon: None,
            tip: String::new(),
            key_tip: String::new(),
            enabled: true,
            checked: false,
        }
    }
    pub fn icon(mut self, icon: RibbonIcon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn tip(mut self, tip: impl Into<String>) -> Self {
        self.tip = tip.into();
        self
    }
    pub fn key_tip(mut self, key_tip: impl Into<String>) -> Self {
        self.key_tip = key_tip.into();
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
}

/// A command's icon: SVG source, drawn as a mask in the ribbon's ink (its
/// shapes' colours are not used), as the suite's own Fluent icons are. Each
/// distinct icon is kept for the life of the process.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RibbonIcon(Arc<[u8]>);

impl RibbonIcon {
    pub fn svg(source: impl Into<Arc<[u8]>>) -> Self {
        RibbonIcon(source.into())
    }
    pub fn source(&self) -> &[u8] {
        &self.0
    }
}

/// The suite's theme now (#1870): light or dark and its colour tokens, so an
/// extension's body can match the suite around it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SuiteTheme {
    pub dark: bool,
    /// The window and document background.
    pub background: Hsla,
    /// The ribbon's and the tab strip's panel.
    pub panel: Hsla,
    pub border: Hsla,
    /// The tab's accent (the Mail tabs').
    pub accent: Hsla,
    pub text: Hsla,
    pub muted_text: Hsla,
    pub selection: Hsla,
}

/// The suite's theme now (see [`SuiteTheme`]).
pub fn suite_theme(cx: &App) -> SuiteTheme {
    crate::ext_ribbon::suite_theme(cx)
}

/// Tells the suite the ribbon a surface describes changed: every window
/// reads [`TabSurface::ribbon`] again before it next draws. Needs no window,
/// so a model or a background task may call it; it never calls back into
/// the surface before it returns.
pub fn ribbon_changed(cx: &mut App) {
    cx.default_global::<RibbonGeneration>().0 += 1;
    cx.refresh_windows();
}

/// How many times [`ribbon_changed`] was called: a cached ribbon read at an
/// older one is read again.
#[derive(Default)]
struct RibbonGeneration(u64);

impl Global for RibbonGeneration {}

pub(crate) fn ribbon_generation(cx: &App) -> u64 {
    cx.try_global::<RibbonGeneration>().map_or(0, |g| g.0)
}

/// Makes the surface of a new tab of one kind. An error is said on the tab's
/// status line and the tab keeps the placeholder.
pub type Factory = Rc<dyn Fn(&mut App) -> Result<Box<dyn TabSurface>, String>>;

/// What a build links in. Every field is optional.
#[derive(Default)]
pub struct Extensions {
    /// Draws Mail tabs (File > New > Mail, a restored Mail tab).
    pub mail: Option<Factory>,
}

struct Installed(Extensions);

impl Global for Installed {}

pub(crate) fn install(cx: &mut App, extensions: Extensions) {
    cx.set_global(Installed(extensions));
}

/// A new Mail surface, when an extension draws Mail: `None` without one.
pub(crate) fn new_mail(cx: &mut App) -> Option<Result<Box<dyn TabSurface>, String>> {
    let factory = cx.try_global::<Installed>()?.0.mail.clone()?;
    Some(factory(cx))
}
