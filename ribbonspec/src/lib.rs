//! ribbonspec — a UI-agnostic model of an Office-style ribbon.
//!
//! One [`Ribbon<A>`] describes an app's command surface — tabs, contextual tabs,
//! groups, controls, and commands — parameterised by the app's own action type
//! `A`. It carries NO rendering: no ratatui, no gpui, no colours, no pixels. Two
//! renderers consume the same value:
//!   * the terminal apps (docxy/xlsxy/… TUIs) draw it with ratatui, and
//!   * the desktop suite draws it with GPUI (Fluent-2 skin).
//! so one ribbon/command definition drives both, and a change lands in both.
//!
//! What the model *does* encode is layout INTENT that a faithful ribbon needs but
//! that is renderer-independent:
//!   * [`Group::priority`] — the order groups shrink/collapse as width drops
//!     (responsive scaling), lowest priority collapsing first.
//!   * [`Control`] size intent (large vs a column of small buttons vs split/…),
//!   * [`ScreenTip`] rich-tooltip text and [`Cmd::key_tip`] Alt-access badges,
//!   * [`ContextTab`] visibility accent for table/picture/… contexts,
//!   * [`Icon`] a logical icon id each renderer maps to its own icon set
//!     (the GPUI renderer maps these to MIT Fluent System Icons).
//!
//! `A` is typically a small `Copy` enum; helpers below keep definitions terse.

#![forbid(unsafe_code)]

/// The whole ribbon for an app.
pub struct Ribbon<A> {
    /// Quick Access Toolbar — a few always-visible commands (Save, Undo, Redo),
    /// rendered in the window chrome above/around the tabs.
    pub qat: Vec<Cmd<A>>,
    /// The persistent tabs (Home, Insert, …), left to right.
    pub tabs: Vec<Tab<A>>,
    /// Contextual tabs, shown only while their context is active.
    pub contextual: Vec<ContextTab<A>>,
}

impl<A> Ribbon<A> {
    pub fn new(tabs: Vec<Tab<A>>) -> Self {
        Ribbon { qat: Vec::new(), tabs, contextual: Vec::new() }
    }
    pub fn qat(mut self, qat: Vec<Cmd<A>>) -> Self {
        self.qat = qat;
        self
    }
    pub fn contextual(mut self, contextual: Vec<ContextTab<A>>) -> Self {
        self.contextual = contextual;
        self
    }
}

/// A persistent ribbon tab.
pub struct Tab<A> {
    pub name: &'static str,
    /// The Alt-access badge (e.g. "H" for Home). Empty = none.
    pub key_tip: &'static str,
    pub groups: Vec<Group<A>>,
}

/// A contextual tab (Table Tools, Picture Tools, …) — shown only in-context.
pub struct ContextTab<A> {
    pub name: &'static str,
    /// The context whose presence reveals this tab.
    pub context: Context,
    /// Accent hue for the tab set header (renderer maps to a colour).
    pub accent: Accent,
    pub key_tip: &'static str,
    pub groups: Vec<Group<A>>,
}

/// A document context that reveals contextual tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Context {
    Table,
    Picture,
    Drawing,
    Chart,
    Header,
    List,
}

/// Contextual-tab accent hue (renderer-mapped, not a literal colour).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Accent {
    Green,
    Blue,
    Orange,
    Purple,
    Red,
    Teal,
}

/// A group of controls within a tab, drawn as a titled card with a divider.
pub struct Group<A> {
    pub title: &'static str,
    /// The dialog-box launcher action (the small ⤢ at the group's corner), if the
    /// group opens an advanced dialog. `None` = no launcher.
    pub launcher: Option<A>,
    /// Responsive-collapse order: as the ribbon runs out of width, groups with the
    /// LOWEST priority shrink/collapse to an overflow popup first. Higher = kept
    /// large longer. (Typical: 0 = least important … 255 = pin.)
    pub priority: u8,
    pub items: Vec<Control<A>>,
}

/// A control placed in a group. The variant encodes *size/kind intent*; the
/// renderer decides exact metrics per its skin.
pub enum Control<A> {
    /// A big icon-over-label button (the group's headline command).
    Large(Cmd<A>),
    /// Up to three small icon+label buttons stacked in a column.
    Column(Vec<Cmd<A>>),
    /// A primary button plus a dropdown of related commands.
    Split { primary: Cmd<A>, menu: Vec<Cmd<A>> },
    /// A labelled dropdown (e.g. Font, Font Size) that opens a list.
    Dropdown { cmd: Cmd<A>, items: Vec<Cmd<A>> },
    /// A gallery of visual choices (e.g. the Styles gallery) with live preview.
    Gallery(Gallery<A>),
    /// A two-state toggle (e.g. Bold/Italic) — `checked` is resolved at render.
    Toggle(Cmd<A>),
    /// A thin vertical separator between controls.
    Separator,
}

/// A gallery control — a scrollable grid/row of visual choices.
pub struct Gallery<A> {
    pub id: &'static str,
    pub tip: ScreenTip,
    pub items: Vec<GalleryItem<A>>,
}

/// One gallery choice. `preview` is a renderer hint (e.g. a style id) for the live
/// preview; `act` applies it.
pub struct GalleryItem<A> {
    pub label: &'static str,
    pub preview: &'static str,
    pub act: A,
}

/// A single command: what a button/menu-item invokes, plus its presentation.
pub struct Cmd<A> {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: Icon,
    pub tip: ScreenTip,
    /// Alt-access badge letters (e.g. "FB" reached via Alt,F,B). Empty = none.
    pub key_tip: &'static str,
    pub act: A,
}

/// A rich tooltip: a bold title, a description, and the keyboard shortcut.
#[derive(Clone, Copy, Default)]
pub struct ScreenTip {
    pub title: &'static str,
    pub body: &'static str,
    pub shortcut: &'static str,
}

/// A logical icon id (e.g. "bold", "align-left"). Each renderer maps it to its own
/// icon set — the GPUI renderer to MIT Fluent System Icons, the TUI to a glyph.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Icon(pub &'static str);

// ---- terse constructors ----------------------------------------------------

/// A command with just an icon id + label + action (no tip/keytip).
pub fn cmd<A>(id: &'static str, icon: &'static str, label: &'static str, act: A) -> Cmd<A> {
    Cmd { id, label, icon: Icon(icon), tip: ScreenTip::default(), key_tip: "", act }
}

impl<A> Cmd<A> {
    /// Attach a rich ScreenTip.
    pub fn tip(mut self, title: &'static str, body: &'static str, shortcut: &'static str) -> Self {
        self.tip = ScreenTip { title, body, shortcut };
        self
    }
    /// Attach an Alt-access KeyTip badge.
    pub fn key(mut self, key_tip: &'static str) -> Self {
        self.key_tip = key_tip;
        self
    }
    pub fn large(self) -> Control<A> {
        Control::Large(self)
    }
    pub fn toggle(self) -> Control<A> {
        Control::Toggle(self)
    }
}

/// A group: `group(title, priority, controls)`.
pub fn group<A>(title: &'static str, priority: u8, items: Vec<Control<A>>) -> Group<A> {
    Group { title, launcher: None, priority, items }
}

impl<A> Group<A> {
    /// Add a dialog-box launcher action.
    pub fn launcher(mut self, act: A) -> Self {
        self.launcher = Some(act);
        self
    }
}

/// A column of up to three small buttons.
pub fn column<A>(cmds: Vec<Cmd<A>>) -> Control<A> {
    Control::Column(cmds)
}

/// A tab: `tab(name, key_tip, groups)`.
pub fn tab<A>(name: &'static str, key_tip: &'static str, groups: Vec<Group<A>>) -> Tab<A> {
    Tab { name, key_tip, groups }
}
