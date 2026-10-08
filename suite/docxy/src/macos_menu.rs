//! The macOS menu bar (#1071).
//!
//! macOS gives an app's Quit (⌘Q), Enter Full Screen (⌃⌘F) and the text
//! fields of native Open and Save panels their Select All, Copy, Paste and
//! Undo through the app's menu bar. The suite had none, so all of them did
//! nothing. [`MENUS`] is the bar, as data, and [`install`] hands it to gpui.
//!
//! Three rules keep the bar from changing what the window already does,
//! with one exception: on macOS ⌘M is Minimize, as in every Mac app, where
//! `on_key` took it for Word's Ctrl+M indent. ⌃M still indents, and ⌘⇧M
//! still outdents.
//!
//! * **A chord reaches one path.** AppKit offers a key to the window first
//!   and to the menu bar only when the window leaves it unhandled. The root's
//!   key listener (`text_input::route`) stops every chord the window owns
//!   ([`Role::Window`]: ⌘N, ⌘S, ⌘C…, which `on_key` already handles), so the
//!   menu item never fires a second time. A chord only the menu bar handles
//!   ([`Role::Menu`]: ⌘Q, ⌘H, ⌘M, ⌘O, ⌘, and ⌃⌘F) skips `on_key` and goes
//!   on to the menu bar; of these, only ⌘M did anything in `on_key`.
//! * **No live binding.** gpui shows an item's shortcut, and AppKit matches
//!   it, from the keymap. Every binding here is in [`MENU_CONTEXT`], which
//!   no element sets, so gpui never matches one in the window: the bindings
//!   take no key from `on_key`.
//! * **A click runs the key.** Clicking a window-owned item types its chord
//!   into `on_key`, so every guard there (an open dialog, a menu, the find
//!   bar, a cell being edited) applies to the click as it does to the key.
//!
//! ⚠️ gpui's menu callbacks re-enter the app with a plain `borrow_mut`. A
//! synchronous native dialog (rfd's `runModal`) runs inside a gpui handler,
//! with the app already borrowed, so a gpui menu would abort the process
//! when the panel validates or opens a menu. Every such dialog runs through
//! [`native_modal`], which swaps in a plain AppKit menu bar for the
//! dialog's lifetime: its Edit items send `undo:`, `cut:`, `copy:`, `paste:`
//! and `selectAll:` to the panel's responder chain through a target of its
//! own, and nothing in it reaches gpui (a nil target would fall back to
//! gpui's app delegate, which deadlocks on an item it did not make).

use gpui::{
    Action, App, Context, Div, InteractiveElement as _, KeyDownEvent, Keystroke, Window, actions,
};

use crate::Docxy;
use crate::help_tab::HelpAct;

actions!(
    docxy_menu,
    [
        MenuAbout,
        MenuSettings,
        MenuHide,
        MenuHideOthers,
        MenuShowAll,
        MenuQuit,
        MenuNew,
        MenuOpen,
        MenuSave,
        MenuSaveAs,
        MenuClose,
        MenuUndo,
        MenuRedo,
        MenuCut,
        MenuCopy,
        MenuPaste,
        MenuSelectAll,
        MenuFind,
        MenuFullScreen,
        MenuMinimize,
        MenuZoom,
        MenuHelp,
    ]
);

/// The key context of every menu binding. No element sets it, so a binding
/// gives its item a key equivalent and is never matched in the window.
pub(crate) const MENU_CONTEXT: &str = "DocxyMenuBar";

/// A menu bar command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cmd {
    About,
    Settings,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    New,
    Open,
    Save,
    SaveAs,
    Close,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Find,
    FullScreen,
    Minimize,
    Zoom,
    Help,
}

/// Who takes a command's chord while a suite window has the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// `on_key` handles it, as it did before the menu bar: the root stops
    /// it, so the menu item does not fire too.
    Window,
    /// Only the menu bar handles it: the root leaves it to AppKit and
    /// `on_key` never sees it.
    Menu,
}

/// One entry of a menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Item(&'static str, Cmd),
    Separator,
}

/// One menu of the bar.
pub(crate) struct MenuSpec {
    pub(crate) name: &'static str,
    pub(crate) entries: &'static [Entry],
}

use Entry::{Item, Separator};

/// The menu bar, left to right. The first menu is the application menu,
/// which macOS titles with the app's name whatever it is called here.
pub(crate) const MENUS: &[MenuSpec] = &[
    MenuSpec {
        name: "docxy",
        entries: &[
            Item("About docxy", Cmd::About),
            Separator,
            Item("Settings…", Cmd::Settings),
            Separator,
            Item("Hide docxy", Cmd::Hide),
            Item("Hide Others", Cmd::HideOthers),
            Item("Show All", Cmd::ShowAll),
            Separator,
            Item("Quit docxy", Cmd::Quit),
        ],
    },
    MenuSpec {
        name: "File",
        entries: &[
            Item("New", Cmd::New),
            Item("Open…", Cmd::Open),
            Separator,
            Item("Save", Cmd::Save),
            Item("Save As…", Cmd::SaveAs),
            Separator,
            Item("Close", Cmd::Close),
        ],
    },
    MenuSpec {
        name: "Edit",
        entries: &[
            Item("Undo", Cmd::Undo),
            Item("Redo", Cmd::Redo),
            Separator,
            Item("Cut", Cmd::Cut),
            Item("Copy", Cmd::Copy),
            Item("Paste", Cmd::Paste),
            Item("Select All", Cmd::SelectAll),
            Separator,
            Item("Find", Cmd::Find),
        ],
    },
    MenuSpec {
        name: "View",
        entries: &[Item("Enter Full Screen", Cmd::FullScreen)],
    },
    MenuSpec {
        name: "Window",
        entries: &[Item("Minimize", Cmd::Minimize), Item("Zoom", Cmd::Zoom)],
    },
    MenuSpec {
        name: "Help",
        entries: &[Item("docxy Help", Cmd::Help)],
    },
];

impl Cmd {
    /// The item's shortcut, in gpui's keystroke syntax.
    pub(crate) fn chord(self) -> Option<&'static str> {
        Some(match self {
            Cmd::Settings => "cmd-,",
            Cmd::Hide => "cmd-h",
            Cmd::HideOthers => "alt-cmd-h",
            Cmd::Quit => "cmd-q",
            Cmd::New => "cmd-n",
            Cmd::Open => "cmd-o",
            Cmd::Save => "cmd-s",
            Cmd::SaveAs => "cmd-shift-s",
            Cmd::Close => "cmd-w",
            Cmd::Undo => "cmd-z",
            Cmd::Redo => "cmd-shift-z",
            Cmd::Cut => "cmd-x",
            Cmd::Copy => "cmd-c",
            Cmd::Paste => "cmd-v",
            Cmd::SelectAll => "cmd-a",
            Cmd::Find => "cmd-f",
            Cmd::FullScreen => "ctrl-cmd-f",
            Cmd::Minimize => "cmd-m",
            Cmd::About | Cmd::ShowAll | Cmd::Zoom | Cmd::Help => return None,
        })
    }

    /// Who takes the chord while a suite window has the key. Window-owned
    /// commands are the ones `on_key` handles on every platform (⌘ is its
    /// Ctrl); a click on one types its chord into `on_key`. Menu-only
    /// chords never reach `on_key` on macOS: ⌘M, its Ctrl+M indent, is
    /// Minimize there (⌃M still indents).
    pub(crate) fn role(self) -> Role {
        match self {
            Cmd::New
            | Cmd::Save
            | Cmd::SaveAs
            | Cmd::Close
            | Cmd::Undo
            | Cmd::Redo
            | Cmd::Cut
            | Cmd::Copy
            | Cmd::Paste
            | Cmd::SelectAll
            | Cmd::Find => Role::Window,
            Cmd::About
            | Cmd::Settings
            | Cmd::Hide
            | Cmd::HideOthers
            | Cmd::ShowAll
            | Cmd::Quit
            | Cmd::Open
            | Cmd::FullScreen
            | Cmd::Minimize
            | Cmd::Zoom
            | Cmd::Help => Role::Menu,
        }
    }

    /// The selector AppKit sends for it, through gpui: the native ones make a
    /// panel's text field cut, copy, paste and select.
    pub(crate) fn os_action(self) -> Option<gpui::OsAction> {
        Some(match self {
            Cmd::Undo => gpui::OsAction::Undo,
            Cmd::Redo => gpui::OsAction::Redo,
            Cmd::Cut => gpui::OsAction::Cut,
            Cmd::Copy => gpui::OsAction::Copy,
            Cmd::Paste => gpui::OsAction::Paste,
            Cmd::SelectAll => gpui::OsAction::SelectAll,
            _ => return None,
        })
    }

    /// The gpui action its item dispatches.
    pub(crate) fn action(self) -> Box<dyn Action> {
        match self {
            Cmd::About => Box::new(MenuAbout),
            Cmd::Settings => Box::new(MenuSettings),
            Cmd::Hide => Box::new(MenuHide),
            Cmd::HideOthers => Box::new(MenuHideOthers),
            Cmd::ShowAll => Box::new(MenuShowAll),
            Cmd::Quit => Box::new(MenuQuit),
            Cmd::New => Box::new(MenuNew),
            Cmd::Open => Box::new(MenuOpen),
            Cmd::Save => Box::new(MenuSave),
            Cmd::SaveAs => Box::new(MenuSaveAs),
            Cmd::Close => Box::new(MenuClose),
            Cmd::Undo => Box::new(MenuUndo),
            Cmd::Redo => Box::new(MenuRedo),
            Cmd::Cut => Box::new(MenuCut),
            Cmd::Copy => Box::new(MenuCopy),
            Cmd::Paste => Box::new(MenuPaste),
            Cmd::SelectAll => Box::new(MenuSelectAll),
            Cmd::Find => Box::new(MenuFind),
            Cmd::FullScreen => Box::new(MenuFullScreen),
            Cmd::Minimize => Box::new(MenuMinimize),
            Cmd::Zoom => Box::new(MenuZoom),
            Cmd::Help => Box::new(MenuHelp),
        }
    }

    /// The chord as a keystroke.
    fn keystroke(self) -> Option<Keystroke> {
        self.chord().and_then(|c| Keystroke::parse(c).ok())
    }
}

/// Every command on the bar, in menu order.
pub(crate) fn commands() -> impl Iterator<Item = Cmd> {
    MENUS.iter().flat_map(|m| {
        m.entries.iter().filter_map(|e| match e {
            Item(_, cmd) => Some(*cmd),
            Separator => None,
        })
    })
}

/// The role of the menu chord `k` is, if it is one: the same key with
/// exactly the same modifiers, so ⌘F is the window's Find and ⌃⌘F the menu
/// bar's Enter Full Screen.
pub(crate) fn chord_role(k: &Keystroke) -> Option<Role> {
    commands().find_map(|cmd| {
        let c = cmd.keystroke()?;
        (c.key == k.key && c.modifiers == k.modifiers).then(|| cmd.role())
    })
}

/// The keystroke `on_key` gets for `k` on macOS, and whether it is a
/// Redo that must not Repeat. ⌘⇧Z is the Mac's Redo, which `on_key` knows
/// as ⌘Y (its Ctrl+Y), but ⌘Y also repeats the last action when there is
/// nothing to redo (Word's and Excel's Repeat, #618) and a Mac's Redo does
/// not. Every other key is itself.
pub(crate) fn mac_alias(k: &Keystroke) -> (Keystroke, bool) {
    let m = &k.modifiers;
    if k.key == "z" && m.platform && m.shift && !m.control && !m.alt && !m.function {
        let mut y = k.clone();
        y.key = "y".into();
        y.key_char = None;
        y.modifiers.shift = false;
        return (y, true);
    }
    (k.clone(), false)
}

/// Whether `k` is Save As: ⌘⇧S, on macOS only. Elsewhere Ctrl+Shift+S
/// stays what it was.
pub(crate) fn is_save_as(k: &Keystroke, macos: bool) -> bool {
    macos
        && Cmd::SaveAs
            .keystroke()
            .is_some_and(|c| c.key == k.key && c.modifiers == k.modifiers)
}

/// Runs a synchronous native dialog (rfd's file and message dialogs) with
/// the menu bar swapped for one that never calls back into gpui; see the
/// module docs. Every such dialog goes through here (a test scans the
/// sources for one that does not).
pub(crate) fn native_modal<R>(run: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "macos")]
    let _swap = native::Swap::enter();
    run()
}

/// The window root's handlers for the window's commands. Off macOS nothing
/// dispatches them.
pub(crate) fn window_actions(root: Div, cx: &mut Context<Docxy>) -> Div {
    macro_rules! on {
        ($root:expr, $($action:ident => $cmd:expr),* $(,)?) => {
            $root$(.on_action(cx.listener(|this, _: &$action, window, cx| {
                this.menu_command($cmd, window, cx)
            })))*
        };
    }
    on!(root,
        MenuAbout => Cmd::About,
        MenuSettings => Cmd::Settings,
        MenuNew => Cmd::New,
        MenuOpen => Cmd::Open,
        MenuSave => Cmd::Save,
        MenuSaveAs => Cmd::SaveAs,
        MenuClose => Cmd::Close,
        MenuUndo => Cmd::Undo,
        MenuRedo => Cmd::Redo,
        MenuCut => Cmd::Cut,
        MenuCopy => Cmd::Copy,
        MenuPaste => Cmd::Paste,
        MenuSelectAll => Cmd::SelectAll,
        MenuFind => Cmd::Find,
        MenuFullScreen => Cmd::FullScreen,
        MenuMinimize => Cmd::Minimize,
        MenuZoom => Cmd::Zoom,
        MenuHelp => Cmd::Help,
    )
}

impl Docxy {
    /// A window command from the menu bar: a click, or the key equivalent of
    /// a menu-only chord.
    pub(crate) fn menu_command(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        if cmd.role() == Role::Window {
            if let Some(keystroke) = cmd.keystroke() {
                self.type_menu_chord(keystroke, window, cx);
            }
            return;
        }
        match cmd {
            Cmd::FullScreen => window.toggle_fullscreen(),
            Cmd::Minimize => window.minimize_window(),
            Cmd::Zoom => window.zoom_window(),
            _ if !self.menu_click_allowed() => {}
            Cmd::Open => self.open_file(window, cx),
            Cmd::Settings => self.open_backstage(cx),
            Cmd::About => self.help_act(HelpAct::About, window, cx),
            Cmd::Help => self.help_act(HelpAct::Help, window, cx),
            _ => {}
        }
    }

    /// A window-owned item's click: its chord, typed into `on_key`.
    fn type_menu_chord(
        &mut self,
        keystroke: Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ev = KeyDownEvent {
            keystroke,
            is_held: false,
            prefer_character_input: false,
        };
        self.ime.forget_key();
        self.window_chord(&ev, window, cx);
    }

    /// A window-owned chord, pressed or clicked, into `on_key` through
    /// [`mac_alias`]; ⌘⇧Z's Redo sets `redo_only` for that one key.
    pub(crate) fn window_chord(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (keystroke, redo_only) = mac_alias(&ev.keystroke);
        self.redo_only = redo_only;
        self.on_key(
            &KeyDownEvent {
                keystroke,
                ..ev.clone()
            },
            window,
            cx,
        );
        self.redo_only = false;
    }

    /// Whether a menu-only item may run: not under an open dialog, which
    /// covers the window, nor an open menu, as a click there could not.
    fn menu_click_allowed(&self) -> bool {
        self.refuse_under_dialog().is_ok() && self.menu.is_none()
    }
}

/// Quit (⌘Q), in two phases (#1071).
///
/// **Asking.** Each window, newest first, agrees through
/// [`Docxy::quit_ask`]: with "ask before closing" on, the last window's
/// close questions about each unsaved tab, otherwise at once. Answers are
/// only recorded: Save saves the tab there and then, as any Save does, but
/// Don't Save forgets nothing yet. A window that asks stops the walk and is
/// brought to the front; its last answer resumes the walk
/// ([`resume_quit`]). Cancel in any window stops the whole quit and drops
/// every window's recorded answers ([`cancel_quit`]): nothing was
/// forgotten, so every tab answered Don't Save keeps its unsaved work.
///
/// **Applying.** Once every window has agreed in one walk (an agreement
/// that no longer holds, because tabs changed or a tab was edited after
/// its Save, is asked again), each window applies its answers and writes
/// the session, the run is marked clean, and the windows go, the last one
/// ending the process (`QuitMode::LastWindowClosed`). No window leaves the
/// registry, so the session keeps every window's tabs.
///
/// ⌘Q again while a window asks for the quit only brings that window to
/// the front. The action handler defers this: a menu action runs inside
/// the active window's update, where that window cannot be updated again.
pub(crate) fn quit(cx: &mut App) {
    let entries = crate::windows::entries_snapshot(cx);
    let asking: Vec<bool> = entries
        .iter()
        .map(|(_, view, _)| {
            view.upgrade().is_some_and(|v| {
                let this = v.read(cx);
                this.app_quit && crate::close::quit_prompt_live(&this.tabs)
            })
        })
        .collect();
    if let Some(i) = pending_quit(&asking) {
        let _ = entries[i]
            .2
            .update(cx, |_, window, _| window.activate_window());
        return;
    }
    cancel_quit(cx);
    resume_quit(cx);
}

/// The window already asking for a Quit, if one is: a repeated ⌘Q waits
/// for it instead of starting over.
pub(crate) fn pending_quit(asking: &[bool]) -> Option<usize> {
    asking.iter().position(|&a| a)
}

/// Drop every window's recorded agreement to a Quit.
pub(crate) fn cancel_quit(cx: &mut App) {
    for (_, view, _) in crate::windows::entries_snapshot(cx) {
        if let Some(view) = view.upgrade() {
            view.update(cx, |this, _| this.quit_agreed = None);
        }
    }
}

/// Go on with a Quit: ask each window in turn; once all have agreed, apply
/// their answers and close them.
pub(crate) fn resume_quit(cx: &mut App) {
    let entries = crate::windows::entries_snapshot(cx);
    for (_, view, handle) in entries.iter().rev() {
        let Some(view) = view.upgrade() else {
            continue;
        };
        let agreed = handle.update(cx, |_, window, cx| {
            let agreed = view.update(cx, |this, cx| this.quit_ask(window, cx));
            if !agreed {
                window.activate_window();
            }
            agreed
        });
        if matches!(agreed, Ok(false)) {
            return;
        }
    }
    let views: Vec<_> = entries.iter().filter_map(|(_, v, _)| v.upgrade()).collect();
    for view in &views {
        view.update(cx, |this, cx| this.apply_quit(cx));
    }
    if let Some(view) = views.first() {
        view.read(cx).mark_clean_exit();
    }
    for (_, _, handle) in entries {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// Install the menu bar: the app-wide handlers, the menu bindings and the
/// bar itself. Built everywhere, so every CI leg type-checks it; only
/// macOS calls it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn install(cx: &mut App) {
    cx.on_action(|_: &MenuQuit, cx| cx.defer(quit));
    cx.on_action(|_: &MenuHide, cx| cx.hide());
    cx.on_action(|_: &MenuHideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &MenuShowAll, cx| cx.unhide_other_apps());
    let bindings: Vec<gpui::KeyBinding> = commands()
        .filter_map(|cmd| {
            let chord = cmd.chord()?;
            let context = gpui::KeyBindingContextPredicate::parse(MENU_CONTEXT).ok()?;
            gpui::KeyBinding::load(
                chord,
                cmd.action(),
                Some(context.into()),
                false,
                None,
                &gpui::DummyKeyboardMapper,
            )
            .ok()
        })
        .collect();
    cx.bind_keys(bindings);
    cx.set_menus(MENUS.iter().map(|m| {
        gpui::Menu::new(m.name).items(m.entries.iter().map(|e| match *e {
            Separator => gpui::MenuItem::separator(),
            Item(name, cmd) => gpui::MenuItem::Action {
                name: name.into(),
                action: cmd.action(),
                os_action: cmd.os_action(),
                checked: false,
                disabled: false,
            },
        }))
    }));
}

/// The menu bar a native dialog runs under: plain AppKit items, no
/// delegate, and Edit items aimed at [`native::EditTarget`], so neither
/// AppKit's validation nor an action ever reaches gpui's app delegate.
#[cfg(target_os = "macos")]
mod native {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
    use objc2::{ClassType as _, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSResponder};
    use objc2_foundation::NSString;

    /// The menu bar that was up, put back on drop, and the target of the
    /// bar put up meanwhile (a menu item does not retain its target).
    pub(super) struct Swap {
        saved: Option<Retained<NSMenu>>,
        _target: Retained<EditTarget>,
    }

    impl Swap {
        /// `None` off the main thread, where no dialog runs modally.
        pub(super) fn enter() -> Option<Swap> {
            let mtm = MainThreadMarker::new()?;
            let app = NSApplication::sharedApplication(mtm);
            let saved = app.mainMenu();
            let target = EditTarget::new(mtm);
            let cmd = NSEventModifierFlags::Command;
            let cmd_shift = cmd | NSEventModifierFlags::Shift;
            let bar = NSMenu::new(mtm);
            let app_menu = NSMenu::new(mtm);
            // NSApplication answers hide: itself, before its delegate.
            app_menu.addItem(&item(mtm, "Hide docxy", sel!(hide:), "h", cmd, None));
            bar.addItem(&holder(mtm, &app_menu));
            let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Edit"));
            for (title, action, key, mask) in [
                ("Undo", sel!(undo:), "z", cmd),
                ("Redo", sel!(redo:), "z", cmd_shift),
                ("Cut", sel!(cut:), "x", cmd),
                ("Copy", sel!(copy:), "c", cmd),
                ("Paste", sel!(paste:), "v", cmd),
                ("Select All", sel!(selectAll:), "a", cmd),
            ] {
                edit.addItem(&item(mtm, title, action, key, mask, Some(&target)));
            }
            bar.addItem(&holder(mtm, &edit));
            app.setMainMenu(Some(&bar));
            Some(Swap {
                saved,
                _target: target,
            })
        }
    }

    impl Drop for Swap {
        fn drop(&mut self) {
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).setMainMenu(self.saved.as_deref());
            }
        }
    }

    define_class!(
        /// The Edit items' target. A nil target would let AppKit fall back
        /// to gpui's app delegate, which answers these selectors and, for an
        /// item it never made, deadlocks on its own lock. This one sends
        /// each action to the key window's responder chain, stopping short
        /// of the application and its delegate, and is the only object
        /// AppKit asks to validate the items.
        // SAFETY: NSObject has no subclassing requirements, and the class
        // has no ivars and no Drop.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        struct EditTarget;

        unsafe impl NSObjectProtocol for EditTarget {}

        impl EditTarget {
            #[unsafe(method(undo:))]
            fn undo(&self, sender: Option<&AnyObject>) {
                self.forward(sel!(undo:), sender);
            }

            #[unsafe(method(redo:))]
            fn redo(&self, sender: Option<&AnyObject>) {
                self.forward(sel!(redo:), sender);
            }

            #[unsafe(method(cut:))]
            fn cut(&self, sender: Option<&AnyObject>) {
                self.forward(sel!(cut:), sender);
            }

            #[unsafe(method(copy:))]
            fn copy(&self, sender: Option<&AnyObject>) {
                self.forward(sel!(copy:), sender);
            }

            #[unsafe(method(paste:))]
            fn paste(&self, sender: Option<&AnyObject>) {
                self.forward(sel!(paste:), sender);
            }

            #[unsafe(method(selectAll:))]
            fn select_all(&self, sender: Option<&AnyObject>) {
                self.forward(sel!(selectAll:), sender);
            }

            #[unsafe(method(validateMenuItem:))]
            fn validate_menu_item(&self, item: &NSMenuItem) -> Bool {
                let Some(responder) = item.action().and_then(|a| responder_for(self.mtm(), a))
                else {
                    return Bool::NO;
                };
                if responder.respondsToSelector(sel!(validateMenuItem:)) {
                    // SAFETY: it answers validateMenuItem:, which takes the
                    // item and returns BOOL.
                    unsafe { msg_send![&*responder, validateMenuItem: item] }
                } else {
                    Bool::YES
                }
            }
        }
    );

    impl EditTarget {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            // SAFETY: NSObject's designated initialiser.
            unsafe { msg_send![super(this), init] }
        }

        fn forward(&self, action: Sel, sender: Option<&AnyObject>) {
            if let Some(responder) = responder_for(self.mtm(), action) {
                // SAFETY: the responder answers `action`, a standard action
                // taking its sender.
                unsafe { responder.tryToPerform_with(action, sender) };
            }
        }
    }

    /// The first responder in the key window's chain that answers `action`,
    /// up to (not including) the application, whose delegate is gpui's.
    fn responder_for(mtm: MainThreadMarker, action: Sel) -> Option<Retained<NSResponder>> {
        let window = NSApplication::sharedApplication(mtm).keyWindow()?;
        let mut next = window.firstResponder();
        while let Some(responder) = next {
            if responder.isKindOfClass(NSApplication::class()) {
                return None;
            }
            if responder.respondsToSelector(action) {
                return Some(responder);
            }
            // SAFETY: a plain getter.
            next = unsafe { responder.nextResponder() };
        }
        None
    }

    fn item(
        mtm: MainThreadMarker,
        title: &str,
        action: Sel,
        key: &str,
        mask: NSEventModifierFlags,
        target: Option<&EditTarget>,
    ) -> Retained<NSMenuItem> {
        // SAFETY: each selector is a standard responder action, which AppKit
        // sends with the item as its sender.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        item.setKeyEquivalentModifierMask(mask);
        // SAFETY: the target outlives the bar (`Swap` holds it).
        unsafe { item.setTarget(target.map(|t| &***t)) };
        item
    }

    fn holder(mtm: MainThreadMarker, menu: &NSMenu) -> Retained<NSMenuItem> {
        let item = NSMenuItem::new(mtm);
        item.setSubmenu(Some(menu));
        item
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn key(chord: &str) -> Keystroke {
        Keystroke::parse(chord).unwrap()
    }

    #[test]
    fn the_bar_has_the_mac_menus_in_order() {
        let names: Vec<_> = MENUS.iter().map(|m| m.name).collect();
        assert_eq!(names, ["docxy", "File", "Edit", "View", "Window", "Help"]);
        let labels = |name: &str| -> Vec<&str> {
            MENUS
                .iter()
                .find(|m| m.name == name)
                .unwrap()
                .entries
                .iter()
                .filter_map(|e| match e {
                    Item(label, _) => Some(*label),
                    Separator => None,
                })
                .collect()
        };
        assert_eq!(
            labels("docxy"),
            [
                "About docxy",
                "Settings…",
                "Hide docxy",
                "Hide Others",
                "Show All",
                "Quit docxy"
            ]
        );
        assert_eq!(
            labels("File"),
            ["New", "Open…", "Save", "Save As…", "Close"]
        );
        assert_eq!(
            labels("Edit"),
            ["Undo", "Redo", "Cut", "Copy", "Paste", "Select All", "Find"]
        );
        assert_eq!(labels("View"), ["Enter Full Screen"]);
        assert_eq!(labels("Window"), ["Minimize", "Zoom"]);
        assert_eq!(labels("Help"), ["docxy Help"]);
    }

    #[test]
    fn every_command_is_on_the_bar_once_with_its_own_action() {
        let cmds: Vec<Cmd> = commands().collect();
        for (i, a) in cmds.iter().enumerate() {
            assert!(!cmds[i + 1..].contains(a), "{a:?} is on the bar twice");
            for b in &cmds[i + 1..] {
                assert!(
                    !a.action().partial_eq(b.action().as_ref()),
                    "{a:?} and {b:?} share an action"
                );
            }
        }
        assert_eq!(cmds.len(), 22);
    }

    #[test]
    fn the_standard_shortcuts_are_there_and_none_is_taken_twice() {
        let mut seen = Vec::new();
        for cmd in commands() {
            if let Some(chord) = cmd.chord() {
                let k = key(chord);
                assert!(!seen.contains(&k), "{chord} is on two items");
                seen.push(k);
            }
        }
        for (cmd, chord) in [
            (Cmd::Quit, "cmd-q"),
            (Cmd::Settings, "cmd-,"),
            (Cmd::Hide, "cmd-h"),
            (Cmd::Close, "cmd-w"),
            (Cmd::Redo, "cmd-shift-z"),
            (Cmd::SaveAs, "cmd-shift-s"),
            (Cmd::Find, "cmd-f"),
            (Cmd::FullScreen, "ctrl-cmd-f"),
            (Cmd::Minimize, "cmd-m"),
        ] {
            assert_eq!(cmd.chord(), Some(chord), "{cmd:?}");
        }
    }

    #[test]
    fn the_edit_items_carry_the_native_selectors() {
        use gpui::OsAction;
        for (cmd, os) in [
            (Cmd::Undo, OsAction::Undo),
            (Cmd::Redo, OsAction::Redo),
            (Cmd::Cut, OsAction::Cut),
            (Cmd::Copy, OsAction::Copy),
            (Cmd::Paste, OsAction::Paste),
            (Cmd::SelectAll, OsAction::SelectAll),
        ] {
            assert!(cmd.os_action() == Some(os), "{cmd:?}");
        }
        let others = commands().filter(|c| c.os_action().is_some()).count();
        assert_eq!(others, 6);
    }

    #[test]
    fn the_window_keeps_the_chords_on_key_handles_and_the_bar_gets_the_rest() {
        for chord in [
            "cmd-n",
            "cmd-s",
            "cmd-shift-s",
            "cmd-w",
            "cmd-z",
            "cmd-shift-z",
            "cmd-x",
            "cmd-c",
            "cmd-v",
            "cmd-a",
            "cmd-f",
        ] {
            assert_eq!(chord_role(&key(chord)), Some(Role::Window), "{chord}");
        }
        for chord in [
            "cmd-q",
            "cmd-h",
            "alt-cmd-h",
            "cmd-m",
            "cmd-o",
            "cmd-,",
            "ctrl-cmd-f",
        ] {
            assert_eq!(chord_role(&key(chord)), Some(Role::Menu), "{chord}");
        }
        // Not on the bar: on_key's own, propagating as before.
        for chord in ["cmd-b", "cmd-y", "ctrl-f", "alt-cmd-f", "cmd-shift-c"] {
            assert_eq!(chord_role(&key(chord)), None, "{chord}");
        }
    }

    #[test]
    fn cmd_shift_z_reaches_on_key_as_redo() {
        let (y, redo_only) = mac_alias(&key("cmd-shift-z"));
        assert!(redo_only, "a Mac's Redo does not fall back to Repeat");
        assert_eq!(y.key, "y");
        assert_eq!(
            y.modifiers,
            Modifiers {
                platform: true,
                ..Default::default()
            }
        );
        for chord in [
            "cmd-z",
            "cmd-y",
            "ctrl-shift-z",
            "alt-cmd-shift-z",
            "cmd-shift-s",
        ] {
            assert_eq!(mac_alias(&key(chord)), (key(chord), false), "{chord}");
        }
    }

    /// ⌘Q again while a window asks for a Quit waits on that window rather
    /// than starting the quit over (#1071).
    #[test]
    fn a_repeated_quit_waits_on_the_window_asking() {
        assert_eq!(pending_quit(&[false, true, false]), Some(1));
        assert_eq!(pending_quit(&[false, false]), None);
        assert_eq!(pending_quit(&[]), None);
    }

    #[test]
    fn cmd_shift_s_is_save_as_on_macos_only() {
        assert!(is_save_as(&key("cmd-shift-s"), true));
        assert!(!is_save_as(&key("cmd-shift-s"), false));
        assert!(!is_save_as(&key("cmd-s"), true));
        assert!(!is_save_as(&key("ctrl-shift-s"), true));
    }

    /// Whether the text ending at `at` is inside the open parentheses of a
    /// `native_modal(` call: walking back, every `(` that is not closed
    /// again before `at` encloses it.
    fn inside_native_modal(text: &str, at: usize) -> bool {
        let mut depth = 0usize;
        for (i, c) in text[..at].char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' if depth > 0 => depth -= 1,
                '(' if text[..i].ends_with("native_modal") => return true,
                _ => {}
            }
        }
        false
    }

    /// The synchronous rfd dialog calls in `text`, as (line, call, whether
    /// it runs inside [`native_modal`]'s parentheses).
    fn dialog_calls(text: &str) -> Vec<(usize, &'static str, bool)> {
        let mut calls = vec![
            ".save_file()",
            ".pick_file()",
            ".pick_files()",
            ".pick_folder()",
            ".pick_folders()",
        ];
        if text.contains("MessageDialog") {
            calls.push(".show()");
        }
        let mut found = Vec::new();
        for call in calls {
            for (at, _) in text.match_indices(call) {
                let line = text[..at].matches('\n').count() + 1;
                found.push((line, call, inside_native_modal(text, at)));
            }
        }
        found.sort();
        found
    }

    #[test]
    fn the_scan_tells_a_wrapped_dialog_from_a_bare_one() {
        let wrapped = "let p = native_modal(|| rfd::FileDialog::new()\n    .save_file());";
        assert_eq!(dialog_calls(wrapped), [(2, ".save_file()", true)]);
        let with_lets = "native_modal(|| {\n    let d = rfd::FileDialog::new();\n    let d = d.add_filter(\"A (*.a)\", &[\"a\"]);\n    d.pick_file()\n})";
        assert_eq!(dialog_calls(with_lets), [(4, ".pick_file()", true)]);
        let bare = "native_modal(|| x);\nlet p = rfd::FileDialog::new().pick_file();";
        assert_eq!(dialog_calls(bare), [(2, ".pick_file()", false)]);
        let tail = "fn a() -> X {\n    native_modal(|| x)\n}\nfn b() {\n    d.save_file()\n}";
        assert_eq!(dialog_calls(tail), [(5, ".save_file()", false)]);
        let arms =
            "match m {\n    A => native_modal(|| d.save_file()),\n    B => d.pick_file(),\n}";
        assert_eq!(
            dialog_calls(arms),
            [(2, ".save_file()", true), (3, ".pick_file()", false)]
        );
        let branches = "if c {\n    native_modal(|| d.save_file())\n} else {\n    d.save_file()\n}";
        assert_eq!(
            dialog_calls(branches),
            [(2, ".save_file()", true), (4, ".save_file()", false)]
        );
        let message = "rfd::MessageDialog::new().show();";
        assert_eq!(dialog_calls(message), [(1, ".show()", false)]);
        assert!(dialog_calls("menu.show();").is_empty());
    }

    /// Every synchronous rfd dialog runs under [`native_modal`]: one that
    /// does not would let a gpui menu callback re-enter the borrowed app and
    /// abort the process on macOS.
    #[test]
    fn every_native_dialog_runs_under_native_modal() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![src];
        let mut checked = 0;
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") || path.ends_with("macos_menu.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (line, call, wrapped) in dialog_calls(&text) {
                assert!(
                    wrapped,
                    "{}:{line}: {call} outside macos_menu::native_modal",
                    path.display()
                );
                checked += 1;
            }
        }
        // The nine there were when the menu bar came (#1071).
        assert!(
            checked >= 9,
            "found only {checked} dialogs: is the scan looking?"
        );
    }
}
