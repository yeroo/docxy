//! Text from the platform's input system: macOS dead keys and IME
//! composition (#1072).
//!
//! macOS delivers a dead key (⌥E on ABC) and an input method's composition as
//! *marked text*: `setMarkedText:` with the provisional "´", then `insertText:`
//! with the composed "é". For a printable key with nothing being composed,
//! gpui runs the window's key listeners first and gives AppKit's input
//! context the key only when they leave it propagating. While text is marked,
//! for keys with no text (arrows, Backspace, Esc) and, under an input-method
//! source (Japanese, Chinese, Korean), for printable keys too, the input
//! context gets the key first and the listeners see it only if it is not used.
//! Either way the input context reaches the app only through a registered
//! [`EntityInputHandler`]. The suite had none, so `on_key` typed the dead key's
//! spacing accent itself and the next letter separately ("´e").
//!
//! So on macOS the window root's key listener ([`Docxy::route_key`]) leaves
//! printable keys to AppKit ([`Route::Defer`]) and the handler here hands what
//! AppKit commits back to [`Docxy::on_key`]. While KeyTips or a menu is up,
//! letters are commands, not text: the root keeps them and the handler tells
//! gpui it takes no text, so an input method does not compose them. A commit that is exactly the
//! deferred key's own text (plain typing, Shift, an ⌥ chord that is not a dead
//! key) replays that key event unchanged, so every surface sees the keystroke
//! it saw before. A composed commit ("é", a CJK phrase) arrives as one typed
//! key per character. Marked text inserts nothing until it is committed.
//!
//! Windows registers the handler too, for WM_CHAR alone (#1139). Text sent as
//! Unicode keyboard input (`SendInput` with `KEYEVENTF_UNICODE`: password
//! managers' auto-type, AutoHotkey, on-screen keyboards) arrives as a
//! `VK_PACKET` key-down, which gpui reports as no key at all, and then a
//! WM_CHAR, which reaches nothing but the handler. TranslateMessage posts a
//! key's WM_CHAR when gpui leaves its key-down unhandled, and posted messages
//! come in order, so the WM_CHARs are the one ordered record of all the text,
//! packets included. They may come late: a queue drained of input alone (gpui
//! does that under load, and automation may queue a key's down and up at
//! once) hands over the key-ups and later key-downs first.
//!
//! So Windows text is typed from the WM_CHARs (#1277), through one queue in
//! key-down order ([`WmChars`]). A text key-down (plain or Shift, or AltGr:
//! Ctrl+Alt that gpui flags as changing the character) is *withheld* from
//! `on_key` while the root has the focus and the app takes text. A plain key
//! is replayed unchanged when its first character comes, so every surface
//! sees the keystroke it always did. An AltGr key types its characters as
//! they come: its own Ctrl+Alt keystroke would reach `on_key`'s Ctrl
//! commands. Under a text field (a dialog, a Project cell or prompt) an
//! AltGr key types only while one is still open (#1162, #1312). A withheld
//! key *holds*: every later key-down, Tab and Shift+Tab (gpui actions)
//! included, waits behind it and is routed once its character has come, so
//! commands keep their place among the characters ("a", "b", Backspace, "c"
//! under a drain). Other keys go to `on_key` at once and owe their text
//! ([`owed_text`]): a debt whose WM_CHAR is dropped. With no root focus
//! there is no handler, so no WM_CHAR, and a key leaves no debt.
//!
//! A character's candidates are the leading entries still owed that are
//! not sure to be paid (they [`may_not_come`], or are past
//! [`WM_CHAR_WAIT`]) and the first sure one after them. It pays the sure one
//! if that starts with it, else the first of the others that does. A
//! character no candidate starts with is a packet's, sent before every key
//! still owed, and is typed in its place. Of the entries before the one
//! paid, the withheld and waiting ones never had their character come (an
//! AltGr dead key's accent makes WM_DEADCHAR) and are dropped, and a typed
//! key's debt stays until it lapses; a key-down cannot tell an AltGr dead
//! key from an AltGr character. A dead key's accent comes in the
//! next key's text: a plain dead key reaches nothing ([`passes_to_on_key`])
//! and owes nothing, and the next key owes the composed "é" or "´x". A
//! WM_CHAR that does not come (an AltGr dead key, a key handled after the
//! root) holds for [`WM_CHAR_WAIT`] at most: a plain key is then replayed
//! as it was before #1277 and owes its text, and an AltGr key still types
//! its character if it comes later. A command right after an AltGr dead key
//! waits that long. A mouse-down on either root releases every held key
//! first, an AltGr key typing its character then, so a click acts after
//! the keys before it. A packet's text against a command sent
//! after it is not ordered: gpui hides the packet's key-down. The handler
//! never accepts text input there ([`accepts_text_input`]), so the Windows
//! IME stays off as it was.
//!
//! A replayed ⌥ key still carries ⌥. On macOS the document and the find bar
//! type its character, but a dialog field swallows ⌥ keys, so the dialog is
//! handed the plain typed key ⌥ made instead ([`modifier_char`], #1162).
//!
//! Linux registers no handler: it would hand it every unhandled printable
//! key, which `on_key` has already typed. Off macOS the root also leaves
//! every key propagating, as it always has: on Windows a handled key-down is
//! never translated, which would lose WM_CHAR and the system's Alt+F4 and
//! Alt+Space. Everything else here builds everywhere, and [`route`] and
//! [`owed_text`] take what they need as arguments, so the macOS and Windows
//! rules are tested on every CI leg.

use std::collections::VecDeque;
use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, Context, EntityInputHandler, KeyDownEvent, Keystroke, Modifiers, Pixels, PlatformInput,
    Point, UTF16Selection, Window,
};

use crate::Docxy;

/// Whether this build defers keys to the input handler.
pub(crate) const MACOS: bool = cfg!(target_os = "macos");

/// Whether this build takes WM_CHAR text through the input handler (#1139).
pub(crate) const WINDOWS: bool = cfg!(target_os = "windows");

/// Whether this build registers the input handler at all.
pub(crate) const HANDLER: bool = MACOS || WINDOWS;

/// Where the window root sends a key-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// Leave it to AppKit's input context, which answers through the input
    /// handler: `insertText:` or `setMarkedText:`.
    Defer,
    /// `on_key` takes it and the platform hears it was handled, so it never
    /// also reaches the input handler as text.
    AppStop,
    /// `on_key` sees it and it still propagates: a Cmd chord that is not on
    /// the menu bar, which AppKit offers to the system (Cmd+`) when the
    /// window leaves it unhandled, and every key off macOS.
    AppPropagate,
    /// Only the menu bar handles it (#1071): `on_key` never sees it and
    /// AppKit hands it to its menu item (Cmd+Q, Cmd+H, Ctrl+Cmd+F).
    MenuBar,
}

/// The route for one key-down. On macOS, printable text with no Ctrl, Cmd or
/// Fn goes to AppKit (the keys gpui itself would let an input method see)
/// while the app `takes_text` (see [`takes_text`]); a held key's repeats stay
/// with `on_key`, as they always have. A Cmd chord on the menu bar goes to
/// whichever of `on_key` and the menu bar owns it, never to both (#1071).
/// Off macOS every key goes to `on_key` and propagates, exactly as before
/// #1072.
pub(crate) fn route(keystroke: &Keystroke, is_held: bool, macos: bool, takes_text: bool) -> Route {
    let m = &keystroke.modifiers;
    if !macos {
        return Route::AppPropagate;
    }
    if m.platform {
        return match crate::macos_menu::chord_role(keystroke) {
            Some(crate::macos_menu::Role::Window) => Route::AppStop,
            Some(crate::macos_menu::Role::Menu) => Route::MenuBar,
            None => Route::AppPropagate,
        };
    }
    let printable = keystroke
        .key_char
        .as_deref()
        .is_some_and(|text| !text.is_empty() && !text.chars().any(char::is_control));
    if takes_text && printable && !m.control && !m.function && !is_held {
        Route::Defer
    } else {
        Route::AppStop
    }
}

/// Whether typed letters are text: always under an open dialog, which takes
/// every key before KeyTips or a menu may (`modal_takes_key`), and otherwise
/// not while KeyTips are up or a menu is open, where `on_key` takes them as
/// commands. KeyTips raised before a dialog opened stay set under it.
pub(crate) fn takes_text(dialog_open: bool, keytips_up: bool, menu_open: bool) -> bool {
    dialog_open || (!keytips_up && !menu_open)
}

/// Whether the handler tells the platform it takes text input. gpui turns an
/// input method on for a handler that does, and on Windows that IME (its
/// composition through WM_IME_*) is not supported yet, so only macOS does.
pub(crate) fn accepts_text_input(macos: bool, takes_text: bool) -> bool {
    macos && takes_text
}

/// How long an unmatched WM_CHAR debt is kept (#1139). Hygiene, not a
/// correctness bound: it is for debts whose WM_CHAR never comes, such as on a
/// platform that sends no WM_CHAR at all. A WM_CHAR held back longer than
/// this is typed again.
pub(crate) const WM_CHAR_KEEP: Duration = Duration::from_secs(60);

/// How long a withheld key holds the keys behind it while its WM_CHAR does
/// not come (#1277). A WM_CHAR comes in the same pump pass as its key-down,
/// or right after gpui's input drain. Past this an entry is no longer sure
/// to be paid, and a debt that [`may_not_come`] is dropped.
pub(crate) const WM_CHAR_WAIT: Duration = Duration::from_millis(250);

/// The WM_CHAR text a Windows key-down will send (#1139): its printable
/// text, or none. Alt alone makes WM_SYSCHAR, not WM_CHAR (AltGr is
/// Ctrl+Alt, which does). A Win chord the shell lets through is translated
/// like any key, so it owes its text. A plain dead key ([`plain_dead_key`])
/// makes WM_DEADCHAR, so it owes nothing. Under AltGr the flag marks every
/// character the modifiers change too, so an AltGr dead key owes its accent
/// ([`may_not_come`]), which never comes. "´" then X owes "´x", which comes
/// as two WM_CHARs.
pub(crate) fn owed_text(ev: &KeyDownEvent) -> Option<String> {
    let m = &ev.keystroke.modifiers;
    if m.alt && !m.control {
        return None;
    }
    if plain_dead_key(ev) {
        return None;
    }
    let keystroke = &ev.keystroke;
    keystroke
        .key_char
        .clone()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
}

/// Whether a key-down is a plain dead key on Windows (#1312): gpui flags a
/// key whose ToUnicode is dead as preferring character input, and with no
/// Ctrl, Alt or Win held nothing else sets that flag. It owes no WM_CHAR
/// ([`owed_text`]) and its accent is part of the next key's composed text,
/// so `on_key` must not type it. The accent must be there to type.
pub(crate) fn plain_dead_key(ev: &KeyDownEvent) -> bool {
    let m = &ev.keystroke.modifiers;
    ev.prefer_character_input
        && !m.control
        && !m.alt
        && !m.platform
        && ev
            .keystroke
            .key_char
            .as_deref()
            .is_some_and(|c| !c.is_empty())
}

/// Whether a text field that swallows AltGr is open: a dialog, or the
/// Project cell editor or prompt (#1312).
pub(crate) fn field_open(dialog_open: bool, project_edit_open: bool) -> bool {
    dialog_open || project_edit_open
}

/// Whether `route_key` hands a key-down to `on_key`: not a plain dead key on
/// Windows (#1312), whose accent comes composed with the next key-down (a
/// space then carries "´" as its text, a letter "´x").
pub(crate) fn passes_to_on_key(ev: &KeyDownEvent, windows: bool) -> bool {
    !(windows && plain_dead_key(ev))
}

/// Whether a key-down's debt may never be paid: a key gpui flags as preferring
/// character input under Ctrl, Alt or Win, which is every AltGr character and
/// also an AltGr dead key, whose WM_DEADCHAR is no WM_CHAR (#1139).
pub(crate) fn may_not_come(ev: &KeyDownEvent) -> bool {
    let m = &ev.keystroke.modifiers;
    ev.prefer_character_input && (m.control || m.alt || m.platform)
}

/// The text AppKit would commit for a key event the root may defer: what the
/// harness's queued real input types for itself, since it never reaches
/// AppKit. `None` for any other event, and for every event off macOS. It
/// assumes the app takes text; a key the root kept instead stopped
/// propagating, and the harness types only a key that propagated.
pub(crate) fn deferred_text(event: &PlatformInput) -> Option<String> {
    match event {
        PlatformInput::KeyDown(ev)
            if route(&ev.keystroke, ev.is_held, MACOS, true) == Route::Defer =>
        {
            ev.keystroke.key_char.clone()
        }
        _ => None,
    }
}

/// The text of the WM_CHAR Windows posts for a key event the root left
/// propagating (withheld or typed): what the harness's queued real input
/// sends the handler when `windows`, since it never passes through the
/// window procedure. `None` for any other event, and for a key that owes no
/// WM_CHAR ([`owed_text`]).
pub(crate) fn char_message_text(event: &PlatformInput, windows: bool) -> Option<String> {
    match event {
        PlatformInput::KeyDown(ev) if windows => owed_text(ev),
        _ => None,
    }
}

/// The keystroke that types `c` with no layout behind it: the key named by
/// its lower case, Shift for an upper-case letter, Space as `space`. `None`
/// for a control character, which no typed key carries as text.
pub(crate) fn char_stroke(c: char) -> Option<Keystroke> {
    if c.is_control() {
        return None;
    }
    if c == ' ' {
        return Some(Keystroke {
            modifiers: Modifiers::default(),
            key: "space".into(),
            key_char: Some(" ".into()),
        });
    }
    Some(Keystroke {
        modifiers: Modifiers {
            shift: c.is_uppercase(),
            ..Default::default()
        },
        key: c.to_lowercase().to_string(),
        key_char: Some(c.to_string()),
    })
}

/// The plain typed key a macOS key-down stands for when ⌥ made its
/// character (#1162): ⌥ with no ⌃, ⌘ or Fn, as AppKit's commit of the key's
/// own text replays it. `None` for any other key, and for every key off
/// macOS: Windows' AltGr text is typed from its WM_CHAR instead
/// ([`Withheld::AltGr`]). A dialog field types what this returns; `edit_key`
/// swallows every other ⌥ key.
pub(crate) fn modifier_char(ev: &KeyDownEvent, macos: bool) -> Option<Keystroke> {
    let m = &ev.keystroke.modifiers;
    if !(macos && m.alt && !m.control && !m.platform && !m.function) {
        return None;
    }
    let mut chars = ev.keystroke.key_char.as_deref()?.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => char_stroke(c),
        _ => None,
    }
}

/// The keystroke an open dialog takes for a key-down: [`modifier_char`]'s
/// plain typed key, else the key-down's own (#1162).
pub(crate) fn dialog_stroke(ev: &KeyDownEvent, macos: bool) -> Keystroke {
    modifier_char(ev, macos).unwrap_or_else(|| ev.keystroke.clone())
}

/// How a Windows text key-down is withheld from `on_key` (#1277).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Withheld {
    /// No Ctrl, Alt or Win (Shift allowed): replayed unchanged when its
    /// first character comes.
    Plain,
    /// Ctrl+Alt (no Win) that gpui flags as changing the character: each
    /// character is typed as it comes, its Ctrl+Alt keystroke never.
    AltGr,
}

/// Whether a Windows key-down that owes text ([`owed_text`]) is one that
/// `route_key` withholds from `on_key` while the root has the focus and the
/// app takes text, and how: see [`Withheld`]. `None` for every other key,
/// which goes to `on_key` at once.
pub(crate) fn withheld(ev: &KeyDownEvent) -> Option<Withheld> {
    let m = &ev.keystroke.modifiers;
    owed_text(ev)?;
    if !m.control && !m.alt && !m.platform && !m.function {
        Some(Withheld::Plain)
    } else if m.control && m.alt && !m.platform && ev.prefer_character_input {
        Some(Withheld::AltGr)
    } else {
        None
    }
}

fn typed(keystroke: Keystroke) -> KeyDownEvent {
    KeyDownEvent {
        keystroke,
        is_held: false,
        prefer_character_input: false,
    }
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// What a queue entry stands for: a key-down, or the action-bound Tab or
/// Shift+Tab, which gpui hands the root as an action, never as a key-down
/// (#1037), and which waits behind a withheld key as any key does (#1277).
#[derive(Clone, Debug)]
pub(crate) enum Input {
    Key(KeyDownEvent),
    Tab { shift: bool },
}

/// Where a queued input is (#1277).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Behind a holding entry, not routed yet.
    Waiting,
    /// `on_key` has had it: its text is a debt whose characters are dropped.
    Typed,
    /// Withheld from `on_key` until its characters come.
    Withheld(Withheld),
}

/// One input in [`WmChars`]'s queue.
#[derive(Clone, Debug)]
struct Entry {
    input: Input,
    /// The text whose WM_CHARs have not come.
    text: String,
    /// When it was queued or, once routed, routed: for [`WM_CHAR_WAIT`] and
    /// [`WM_CHAR_KEEP`].
    at: Instant,
    /// [`may_not_come`].
    may_not_come: bool,
    /// Past [`WM_CHAR_WAIT`]: no longer sure to be paid.
    late: bool,
    /// An AltGr key withheld under a text field: it types only while one is
    /// still open (#1162, #1312).
    field: bool,
    /// Withheld with its first character still to come: every later input
    /// waits behind it.
    holds: bool,
    state: State,
}

impl Entry {
    fn new(input: Input, now: Instant) -> Self {
        let (text, may_not_come) = match &input {
            Input::Key(ev) => (owed_text(ev).unwrap_or_default(), may_not_come(ev)),
            Input::Tab { .. } => (String::new(), false),
        };
        Entry {
            input,
            text,
            at: now,
            may_not_come,
            late: false,
            field: false,
            holds: false,
            state: State::Waiting,
        }
    }

    fn sure(&self) -> bool {
        !self.may_not_come && !self.late
    }
}

/// What [`WmChars`] needs of the app: the state that decides a key's route
/// and the handlers it routes to. `Docxy` is one over its window; tests
/// record what reaches it.
pub(crate) trait KeyHost {
    /// Whether a WM_CHAR can reach the input handler: gpui registers it only
    /// while the root has the focus.
    fn focused(&mut self) -> bool;
    /// [`takes_text`].
    fn takes_text(&mut self) -> bool;
    /// [`field_open`].
    fn text_field_open(&mut self) -> bool;
    fn on_key(&mut self, ev: &KeyDownEvent);
    /// The action-bound Tab, or Shift+Tab when `shift`.
    fn tab(&mut self, shift: bool);
}

/// The Windows text queue (#1139, #1277): every key-down and Tab in order,
/// what each still owes in WM_CHAR, and the keys withheld from `on_key`
/// until their characters come. See the module doc. Off Windows
/// (`windows` false) nothing is withheld, and a key `on_key` typed owes
/// its text only for the harness's WM_CHAR twins.
#[derive(Clone, Debug)]
pub(crate) struct WmChars {
    windows: bool,
    entries: VecDeque<Entry>,
    /// A timer for [`WmChars::release_lapsed`] is armed.
    armed: bool,
}

impl Default for WmChars {
    fn default() -> Self {
        Self::new(WINDOWS)
    }
}

impl WmChars {
    pub(crate) fn new(windows: bool) -> Self {
        WmChars {
            windows,
            entries: VecDeque::new(),
            armed: false,
        }
    }

    /// A key-down the root's listener got at `now`.
    pub(crate) fn key_down(&mut self, ev: &KeyDownEvent, now: Instant, host: &mut impl KeyHost) {
        self.input(Input::Key(ev.clone()), now, host);
    }

    /// The action-bound Tab (Shift+Tab when `shift`) at `now`.
    pub(crate) fn tab(&mut self, shift: bool, now: Instant, host: &mut impl KeyHost) {
        self.input(Input::Tab { shift }, now, host);
    }

    fn input(&mut self, input: Input, now: Instant, host: &mut impl KeyHost) {
        self.release_lapsed(now, host);
        let entry = Entry::new(input, now);
        if self.holding() {
            self.entries.push_back(entry);
        } else if let Some(entry) = self.route(entry, now, host) {
            self.entries.push_back(entry);
        }
    }

    /// Whether input is held: a withheld key waits for its first character,
    /// and maybe inputs behind it.
    pub(crate) fn holding(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.holds || e.state == State::Waiting)
    }

    /// How long from `now` until the holding entry lapses, if one holds.
    pub(crate) fn next_lapse(&self, now: Instant) -> Option<Duration> {
        self.entries
            .iter()
            .find(|e| e.holds)
            .map(|e| (e.at + WM_CHAR_WAIT).saturating_duration_since(now))
    }

    /// The WM_CHAR characters still owed, by typed and withheld keys and by
    /// the keys waiting behind them.
    pub(crate) fn owed(&self) -> usize {
        self.entries.iter().map(|e| e.text.chars().count()).sum()
    }

    /// Routes one input now: a Tab runs; a text key is withheld while the
    /// root has the focus and the app takes text (Windows only); any other
    /// key goes to `on_key`, and owes its text while the root has the focus.
    /// The entry to keep, if it still owes text.
    fn route(&self, mut e: Entry, now: Instant, host: &mut impl KeyHost) -> Option<Entry> {
        let ev = match &e.input {
            Input::Tab { shift } => {
                host.tab(*shift);
                return None;
            }
            Input::Key(ev) => ev.clone(),
        };
        // A Windows dead key types nothing: the composed text comes with the
        // next key (#1312).
        if !passes_to_on_key(&ev, self.windows) {
            return None;
        }
        e.at = now;
        // No handler, so no WM_CHAR: nothing to wait for, nothing owed.
        if !host.focused() {
            host.on_key(&ev);
            return None;
        }
        if self.windows && host.takes_text() {
            if let Some(how) = withheld(&ev) {
                e.field = how == Withheld::AltGr && host.text_field_open();
                e.state = State::Withheld(how);
                e.holds = true;
                return Some(e);
            }
        }
        host.on_key(&ev);
        e.state = State::Typed;
        (!e.text.is_empty()).then_some(e)
    }

    /// Routes the waiting inputs, in order, up to the next one that holds.
    fn release(&mut self, now: Instant, host: &mut impl KeyHost) {
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].holds {
                return;
            }
            if self.entries[i].state != State::Waiting {
                i += 1;
                continue;
            }
            let Some(e) = self.entries.remove(i) else {
                return;
            };
            if let Some(e) = self.route(e, now, host) {
                let holds = e.holds;
                self.entries.insert(i, e);
                if holds {
                    return;
                }
                i += 1;
            }
        }
    }

    /// A withheld key stops holding before its character came: a plain key
    /// is replayed now, as it was before #1277, and owes its text; an AltGr
    /// key still types its character if it comes.
    fn unhold(e: &mut Entry, host: &mut impl KeyHost) {
        e.holds = false;
        if e.state == State::Withheld(Withheld::Plain) {
            e.state = State::Typed;
            if let Input::Key(ev) = &e.input {
                if host.takes_text() {
                    host.on_key(ev);
                }
            }
        }
    }

    /// At `now`: an entry past [`WM_CHAR_WAIT`] is no longer sure, a
    /// holding one stops holding ([`WmChars::unhold`]), and a typed key's
    /// debt that [`may_not_come`] is dropped, so a later packet's character
    /// is never swallowed by it. Anything past [`WM_CHAR_KEEP`] goes. Then
    /// the inputs behind are routed.
    pub(crate) fn release_lapsed(&mut self, now: Instant, host: &mut impl KeyHost) {
        for e in &mut self.entries {
            if e.state == State::Waiting || now.saturating_duration_since(e.at) <= WM_CHAR_WAIT {
                continue;
            }
            e.late = true;
            if e.holds {
                Self::unhold(e, host);
            }
        }
        self.entries.retain(|e| {
            e.state == State::Waiting
                || !(e.state == State::Typed && e.may_not_come && e.late)
                    && now.saturating_duration_since(e.at) <= WM_CHAR_KEEP
        });
        self.release(now, host);
    }

    /// Releases every held input now, in order, so a mouse-down acts after
    /// the keys before it: each withheld key stops holding, a plain key
    /// replayed as at the time limit ([`WmChars::release_lapsed`]), and an
    /// AltGr key whose character has not come types its text now (under the
    /// field rule) and owes it, so its late WM_CHAR is dropped. An AltGr dead
    /// key then types its accent: a click ends a dead key.
    pub(crate) fn flush(&mut self, now: Instant, host: &mut impl KeyHost) {
        while self.holding() {
            for e in &mut self.entries {
                if !e.holds {
                    continue;
                }
                if e.state == State::Withheld(Withheld::AltGr) {
                    let types = if e.field {
                        host.text_field_open()
                    } else {
                        host.takes_text()
                    };
                    if types {
                        for k in e.text.chars().filter_map(char_stroke) {
                            host.on_key(&typed(k));
                        }
                    }
                    e.holds = false;
                    e.state = State::Typed;
                } else {
                    Self::unhold(e, host);
                }
            }
            self.release(now, host);
        }
    }

    /// Windows' WM_CHAR at `now` (a surrogate pair joined by gpui): each
    /// character pays its entry and acts there, or is typed as a packet's.
    /// See the module doc. Empty text (an IME message gpui forwards)
    /// changes nothing but what has lapsed.
    pub(crate) fn char_message(&mut self, text: &str, now: Instant, host: &mut impl KeyHost) {
        self.release_lapsed(now, host);
        for c in text.chars() {
            self.char(c, now, host);
            self.release(now, host);
        }
    }

    fn char(&mut self, c: char, now: Instant, host: &mut impl KeyHost) {
        // The candidates: the leading entries owed that may not come, and
        // the first sure one; the sure one first.
        let mut sure = None;
        let mut unsure = None;
        for (i, e) in self.entries.iter().enumerate() {
            if e.text.is_empty() {
                continue;
            }
            if e.sure() {
                sure = Some(i);
                break;
            }
            if unsure.is_none() && e.text.starts_with(c) {
                unsure = Some(i);
            }
        }
        let paid = sure
            .filter(|&i| self.entries[i].text.starts_with(c))
            .or(unsure);
        let Some(mut i) = paid else {
            // A packet's, sent before every key still owed.
            if host.takes_text() {
                if let Some(k) = char_stroke(c) {
                    host.on_key(&typed(k));
                }
            }
            return;
        };
        // Before it: the inputs that waited run, and the withheld or waiting
        // candidates whose character never came (AltGr dead keys) go. A
        // typed key's debt stays until it lapses: it may be a packet's
        // character that came first.
        let mut j = 0;
        while j < i {
            let e = &self.entries[j];
            if e.state == State::Typed || (e.text.is_empty() && e.state != State::Waiting) {
                j += 1;
                continue;
            }
            let Some(e) = self.entries.remove(j) else {
                return;
            };
            i -= 1;
            if e.text.is_empty() {
                // A waiting command: `route` keeps only an entry that owes text.
                let kept = self.route(e, now, host);
                debug_assert!(kept.is_none());
            }
        }
        if self.entries[i].state == State::Waiting {
            let Some(e) = self.entries.remove(i) else {
                return;
            };
            match self.route(e, now, host) {
                Some(e) => self.entries.insert(i, e),
                // `on_key` had it and nothing is owed: the character is its.
                None => return,
            }
        }
        let e = &mut self.entries[i];
        match e.state {
            State::Withheld(Withheld::Plain) => Self::unhold(e, host),
            State::Withheld(Withheld::AltGr) => {
                e.holds = false;
                let types = if e.field {
                    host.text_field_open()
                } else {
                    host.takes_text()
                };
                if types {
                    if let Some(k) = char_stroke(c) {
                        host.on_key(&typed(k));
                    }
                }
            }
            State::Typed | State::Waiting => {}
        }
        e.text.drain(..c.len_utf8());
        if e.text.is_empty() {
            self.entries.remove(i);
        }
    }
}

/// The composition in progress and the key AppKit has been handed, and on
/// Windows the WM_CHAR queue.
#[derive(Clone, Debug, Default)]
pub(crate) struct ImeState {
    /// The provisional text (`setMarkedText:`), shown nowhere yet; `None`
    /// when nothing is being composed.
    marked: Option<String>,
    /// The last key left to AppKit, until its text comes back, it becomes
    /// marked text, or another key reaches the root's listener. (A key the
    /// input context takes first, during a composition, does not clear it,
    /// and `mark` has by then.)
    pending: Option<KeyDownEvent>,
    /// Windows: the key-downs and their WM_CHARs, in order (#1139, #1277).
    wm_chars: WmChars,
}

impl ImeState {
    /// A key was left to AppKit.
    pub(crate) fn defer(&mut self, ev: &KeyDownEvent) {
        self.pending = Some(ev.clone());
    }

    /// A key went to `on_key`: whatever AppKit commits next is not its text.
    pub(crate) fn forget_key(&mut self) {
        self.pending = None;
    }

    /// The WM_CHAR characters still owed.
    pub(crate) fn owed(&self) -> usize {
        self.wm_chars.owed()
    }

    pub(crate) fn marked(&self) -> Option<&str> {
        self.marked.as_deref()
    }

    /// The marked text's range in UTF-16 units: the whole of the text the
    /// handler shows AppKit.
    pub(crate) fn marked_range(&self) -> Option<Range<usize>> {
        self.marked.as_deref().map(|m| 0..utf16_len(m))
    }

    /// The length the handler reports for its text, which is the marked text.
    pub(crate) fn len_utf16(&self) -> usize {
        self.marked.as_deref().map_or(0, utf16_len)
    }

    /// `setMarkedText:`. Empty text ends the composition (an Esc or a
    /// Backspace through it). The key that started it is now part of the
    /// composition, not a key to replay.
    pub(crate) fn mark(&mut self, text: &str) {
        self.marked = (!text.is_empty()).then(|| text.to_string());
        self.pending = None;
    }

    /// `insertText:`: the key events that type `text`, which ends any
    /// composition. The deferred key itself when `text` is its own text,
    /// else one typed key per character.
    pub(crate) fn commit(&mut self, text: &str) -> Vec<KeyDownEvent> {
        self.marked = None;
        if let Some(ev) = self
            .pending
            .take()
            .filter(|ev| ev.keystroke.key_char.as_deref() == Some(text))
        {
            return vec![ev];
        }
        text.chars().filter_map(char_stroke).map(typed).collect()
    }

    /// `unmarkText`: the marked text is accepted as it stands.
    pub(crate) fn unmark(&mut self) -> Vec<KeyDownEvent> {
        match self.marked.take() {
            Some(text) => self.commit(&text),
            None => Vec::new(),
        }
    }
}

impl Docxy {
    /// The window root's key-down listener: see [`route`].
    pub(crate) fn route_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The handler is registered only while the root has the focus, so a
        // key deferred without it would reach no one.
        let takes = self.focus.is_focused(window) && self.takes_text();
        match route(&ev.keystroke, ev.is_held, MACOS, takes) {
            // Left propagating, so gpui hands it to AppKit's input context,
            // which answers through the handler below.
            Route::Defer => self.ime.defer(ev),
            Route::AppStop => {
                self.ime.forget_key();
                self.window_chord(ev, window, cx);
                cx.stop_propagation();
            }
            Route::AppPropagate => {
                self.ime.forget_key();
                let mut q = self.take_wm_chars();
                q.key_down(ev, Instant::now(), &mut Host::new(self, window, cx));
                self.put_wm_chars(q, window, cx);
            }
            Route::MenuBar => self.ime.forget_key(),
        }
    }

    /// AppKit's `insertText:` (#1072): see [`ImeState::commit`].
    pub(crate) fn macos_commit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.ime.commit(text);
        self.type_committed(keys, window, cx);
    }

    /// Windows' WM_CHAR (#1139): see [`WmChars::char_message`]. Text is
    /// typed only while the app takes it; under KeyTips or a menu its
    /// letters would be commands.
    pub(crate) fn windows_char(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut q = self.take_wm_chars();
        q.char_message(text, Instant::now(), &mut Host::new(self, window, cx));
        self.put_wm_chars(q, window, cx);
        cx.notify();
    }

    /// The action-bound Tab, or Shift+Tab when `shift`: at once, or behind
    /// a withheld key's text (#1277).
    pub(crate) fn queued_tab(&mut self, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut q = self.take_wm_chars();
        q.tab(shift, Instant::now(), &mut Host::new(self, window, cx));
        self.put_wm_chars(q, window, cx);
    }

    /// A mouse-down: the held keys go first, so the press acts after the
    /// keys typed before it (#1277).
    pub(crate) fn flush_wm_chars(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ime.wm_chars.holding() {
            return;
        }
        let mut q = self.take_wm_chars();
        q.flush(Instant::now(), &mut Host::new(self, window, cx));
        self.put_wm_chars(q, window, cx);
        cx.notify();
    }

    /// Whether a WM_CHAR for a key-down now would reach the input handler,
    /// which gpui registers only while the root has the focus: what the
    /// harness's WM_CHAR twins follow.
    pub(crate) fn wm_char_reaches(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// The queue, out of `self` while its host (`self`) runs keys.
    fn take_wm_chars(&mut self) -> WmChars {
        std::mem::take(&mut self.ime.wm_chars)
    }

    /// The queue back, and its lapse timer armed while a key holds.
    fn put_wm_chars(&mut self, q: WmChars, window: &mut Window, cx: &mut Context<Self>) {
        let nested = std::mem::replace(&mut self.ime.wm_chars, q);
        // Nothing queues while the queue is out; keep it if anything did.
        self.ime.wm_chars.entries.extend(nested.entries);
        let q = &mut self.ime.wm_chars;
        let Some(wait) = q.next_lapse(Instant::now()) else {
            return;
        };
        if q.armed {
            return;
        }
        q.armed = true;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(wait + Duration::from_millis(1))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.ime.wm_chars.armed = false;
                let mut q = this.take_wm_chars();
                q.release_lapsed(Instant::now(), &mut Host::new(this, window, cx));
                this.put_wm_chars(q, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// A text field that swallows AltGr is open: a dialog, or the Project
    /// cell editor or prompt (#1312).
    fn text_field_open(&self) -> bool {
        field_open(self.dialog_open(), self.project_edit_open())
    }

    fn dialog_open(&self) -> bool {
        self.active_dialogs().is_some_and(|d| d.is_open())
    }

    fn takes_text(&self) -> bool {
        takes_text(
            self.dialog_open(),
            self.keytips != crate::KeyTip::Off,
            self.menu.is_some(),
        )
    }

    fn type_committed(
        &mut self,
        keys: Vec<KeyDownEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for ev in &keys {
            self.on_key(ev, window, cx);
        }
        cx.notify();
    }
}

/// `Docxy` as the queue's [`KeyHost`], over its window.
struct Host<'a, 'b> {
    app: &'a mut Docxy,
    window: &'a mut Window,
    cx: &'a mut Context<'b, Docxy>,
}

impl<'a, 'b> Host<'a, 'b> {
    fn new(app: &'a mut Docxy, window: &'a mut Window, cx: &'a mut Context<'b, Docxy>) -> Self {
        Host { app, window, cx }
    }
}

impl KeyHost for Host<'_, '_> {
    fn focused(&mut self) -> bool {
        self.app.wm_char_reaches(self.window)
    }

    fn takes_text(&mut self) -> bool {
        self.app.takes_text()
    }

    fn text_field_open(&mut self) -> bool {
        self.app.text_field_open()
    }

    fn on_key(&mut self, ev: &KeyDownEvent) {
        self.app.on_key(ev, self.window, self.cx);
    }

    fn tab(&mut self, shift: bool) {
        if shift {
            self.app.shift_tab_key(self.window, self.cx);
        } else {
            self.app.tab_key(self.window, self.cx);
        }
    }
}

impl EntityInputHandler for Docxy {
    // The handler shows AppKit only the marked text: the document and the
    // fields keep their own text and offsets (CLAUDE.md: editor offsets stay
    // logical), and every commit reaches them as typed keys.
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // The caret sits after the marked text.
        let end = self.ime.len_utf16();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.ime.marked_range()
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.ime.unmark();
        self.type_committed(keys, window, cx);
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if MACOS {
            self.macos_commit(text, window, cx);
        } else {
            self.windows_char(text, window, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime.mark(new_text);
        cx.notify();
    }

    // No caret rectangle yet: the candidate window takes AppKit's default
    // place until marked text is drawn inline.
    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    // Under an input-method source gpui hands printable keys to the input
    // context before the root only while this is true; on Windows it turns
    // the IME on, which is why it stays false there.
    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        accepts_text_input(MACOS, self.takes_text())
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.ime.len_utf16())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    fn plain(key: &str, key_char: &str) -> Keystroke {
        stroke(key, Some(key_char), Modifiers::default())
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Default::default()
        }
    }

    fn key_chars(keys: &[KeyDownEvent]) -> Vec<&str> {
        keys.iter()
            .map(|k| k.keystroke.key_char.as_deref().unwrap_or(""))
            .collect()
    }

    #[test]
    fn printable_keys_defer_to_text_input_on_macos() {
        // ⌥E on ABC: the dead key's spacing accent is its key_char.
        assert_eq!(
            route(&stroke("e", Some("´"), alt()), false, true, true),
            Route::Defer
        );
        assert_eq!(route(&plain("a", "a"), false, true, true), Route::Defer);
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            route(&stroke("a", Some("A"), shift), false, true, true),
            Route::Defer
        );
        // Space too: AppKit hands " " back and it replays as the same key,
        // and ⌥E then Space commits the accent itself.
        assert_eq!(route(&plain("space", " "), false, true, true), Route::Defer);
    }

    #[test]
    fn other_keys_stay_with_on_key_and_stop() {
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        let function = Modifiers {
            function: true,
            ..Default::default()
        };
        for (what, k) in [
            ("ctrl+a", stroke("a", Some("a"), ctrl)),
            ("fn key", stroke("a", Some("a"), function)),
            ("left", stroke("left", None, Modifiers::default())),
            ("alt+left", stroke("left", None, alt())),
            ("escape", stroke("escape", None, Modifiers::default())),
            ("enter", stroke("enter", Some("\r"), Modifiers::default())),
            ("tab", stroke("tab", Some("\t"), Modifiers::default())),
            ("backspace", stroke("backspace", None, Modifiers::default())),
        ] {
            assert_eq!(route(&k, false, true, true), Route::AppStop, "{what}");
        }
        // A held key's repeats are typed by on_key, as before.
        assert_eq!(route(&plain("a", "a"), true, true, true), Route::AppStop);
    }

    /// On macOS a Cmd chord on the menu bar reaches one path (#1071): one
    /// `on_key` handles stops, so its menu item does not run it again, and a
    /// menu-only one skips `on_key` for the menu bar. Off macOS every Cmd
    /// chord still goes to `on_key` and propagates.
    #[test]
    fn cmd_chords_reach_either_on_key_or_the_menu_bar() {
        let cmd = Modifiers {
            platform: true,
            ..Default::default()
        };
        let cmd_shift = Modifiers { shift: true, ..cmd };
        let ctrl_cmd = Modifiers {
            control: true,
            ..cmd
        };
        let window = [
            stroke("c", Some("c"), cmd),
            stroke("v", Some("v"), cmd),
            stroke("z", Some("z"), cmd),
            stroke("z", Some("Z"), cmd_shift),
            stroke("s", Some("S"), cmd_shift),
            stroke("f", Some("f"), cmd),
            stroke("n", Some("n"), cmd),
        ];
        let menu = [
            stroke("q", Some("q"), cmd),
            stroke("h", Some("h"), cmd),
            stroke("m", Some("m"), cmd),
            stroke("o", Some("o"), cmd),
            stroke("f", Some("f"), ctrl_cmd),
        ];
        for k in &window {
            assert_eq!(route(k, false, true, true), Route::AppStop, "{k}");
        }
        for k in &menu {
            assert_eq!(route(k, false, true, true), Route::MenuBar, "{k}");
        }
        // Not on the menu bar: on_key's own, still offered to the system.
        assert_eq!(
            route(&stroke("b", Some("b"), cmd), false, true, true),
            Route::AppPropagate
        );
        for k in window.iter().chain(&menu) {
            assert_eq!(route(k, false, false, true), Route::AppPropagate, "{k}");
        }
    }

    #[test]
    /// Off macOS every key reaches `on_key` and propagates, as before: a
    /// stopped key-down on Windows is never translated (no WM_CHAR, no
    /// Alt+F4).
    fn every_key_propagates_off_macos() {
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        for k in [
            plain("a", "a"),
            stroke("e", Some("´"), alt()),
            plain("space", " "),
            stroke("f4", None, alt()),
            stroke("space", Some(" "), alt()),
            stroke("a", Some("a"), ctrl),
            stroke("left", None, Modifiers::default()),
            stroke("enter", Some("\r"), Modifiers::default()),
        ] {
            for takes in [true, false] {
                assert_eq!(
                    route(&k, false, false, takes),
                    Route::AppPropagate,
                    "{}",
                    k.key
                );
                assert_eq!(
                    route(&k, true, false, takes),
                    Route::AppPropagate,
                    "{}",
                    k.key
                );
            }
        }
    }

    /// KeyTips' and menus' letters are commands: the root keeps them, so no
    /// input method composes them.
    #[test]
    fn letters_stay_with_on_key_while_keytips_or_a_menu_is_up() {
        assert!(takes_text(false, false, false));
        assert!(!takes_text(false, true, false));
        assert!(!takes_text(false, false, true));
        assert!(!takes_text(false, true, true));
        for k in [
            plain("h", "h"),
            stroke("e", Some("´"), alt()),
            plain("space", " "),
        ] {
            assert_eq!(
                route(&k, false, true, takes_text(false, true, false)),
                Route::AppStop,
                "{}",
                k.key
            );
            assert_eq!(
                route(&k, false, true, takes_text(false, false, true)),
                Route::AppStop,
                "{}",
                k.key
            );
        }
    }

    /// An open dialog takes every key first, so its fields compose even with
    /// KeyTips left up (F10, then File > Settings > User name... by mouse)
    /// or a menu still set under it.
    #[test]
    fn an_open_dialog_takes_text_over_keytips_and_menus() {
        for (keytips, menu) in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(
                takes_text(true, keytips, menu),
                "keytips {keytips}, menu {menu}"
            );
            assert_eq!(
                route(
                    &stroke("e", Some("´"), alt()),
                    false,
                    true,
                    takes_text(true, keytips, menu)
                ),
                Route::Defer
            );
        }
    }

    /// The bug: ⌥E then E types one "é", and the accent alone types nothing.
    #[test]
    fn a_dead_key_marks_and_the_composed_letter_commits() {
        let mut ime = ImeState::default();
        ime.defer(&typed(stroke("e", Some("´"), alt())));
        ime.mark("´");
        assert_eq!(ime.marked(), Some("´"));
        assert_eq!(ime.marked_range(), Some(0..1));
        assert_eq!(ime.len_utf16(), 1);
        // The second E is composing, so AppKit gets it before the app does.
        let keys = ime.commit("é");
        assert_eq!(key_chars(&keys), ["é"]);
        assert_eq!(keys[0].keystroke.key, "é");
        assert_eq!(keys[0].keystroke.modifiers, Modifiers::default());
        assert_eq!(ime.marked(), None);
        assert_eq!(ime.marked_range(), None);
    }

    #[test]
    fn a_commit_of_the_deferred_keys_own_text_replays_that_key() {
        let mut ime = ImeState::default();
        let shift_a = typed(stroke(
            "a",
            Some("A"),
            Modifiers {
                shift: true,
                ..Default::default()
            },
        ));
        ime.defer(&shift_a);
        assert_eq!(ime.commit("A"), [shift_a]);
        // Project's Alt+Shift+Minus (⌥⇧- types "—" on macOS) keeps its
        // modifiers and its key, so its binding still fires.
        let alt_shift_minus = typed(stroke(
            "-",
            Some("—"),
            Modifiers {
                alt: true,
                shift: true,
                ..Default::default()
            },
        ));
        ime.defer(&alt_shift_minus);
        assert_eq!(ime.commit("—"), [alt_shift_minus]);
        // Replayed once: a later commit is not that key again.
        assert_eq!(ime.commit("—")[0].keystroke.modifiers, Modifiers::default());
    }

    #[test]
    fn a_commit_of_other_text_types_each_character() {
        let mut ime = ImeState::default();
        // ⌥E then X: AppKit commits the accent and the letter together.
        ime.defer(&typed(plain("x", "x")));
        let keys = ime.commit("´x");
        assert_eq!(key_chars(&keys), ["´", "x"]);
        assert_eq!(keys[1].keystroke.key, "x");
        // A capital is typed with Shift, Space as `space`, control
        // characters not at all.
        let keys = ime.commit("É \nb");
        assert_eq!(key_chars(&keys), ["É", " ", "b"]);
        assert!(keys[0].keystroke.modifiers.shift);
        assert_eq!(keys[0].keystroke.key, "é");
        assert_eq!(keys[1].keystroke.key, "space");
    }

    #[test]
    fn a_key_on_key_took_is_not_replayed() {
        let mut ime = ImeState::default();
        ime.defer(&typed(stroke("-", Some("—"), alt())));
        ime.forget_key();
        let keys = ime.commit("—");
        assert_eq!(keys, [typed(plain("—", "—"))]);
        // A key that became marked text is part of the composition: the
        // commit types the accent, not the ⌥E it came from.
        ime.defer(&typed(stroke("e", Some("´"), alt())));
        ime.mark("´");
        let keys = ime.commit("´");
        assert_eq!(keys, [typed(plain("´", "´"))]);
        assert_eq!(keys[0].keystroke.modifiers, Modifiers::default());
    }

    #[test]
    fn unmark_accepts_the_marked_text_and_empty_marked_text_ends_it() {
        let mut ime = ImeState::default();
        ime.mark("´");
        assert_eq!(key_chars(&ime.unmark()), ["´"]);
        assert_eq!(ime.marked(), None);
        assert!(ime.unmark().is_empty());
        // Esc through a composition marks "": nothing is left to commit.
        ime.mark("´");
        ime.mark("");
        assert_eq!(ime.marked(), None);
        assert_eq!(ime.marked_range(), None);
        assert!(ime.unmark().is_empty());
    }

    #[test]
    fn marked_ranges_count_utf16_units() {
        let mut ime = ImeState::default();
        ime.mark("𝄞か");
        assert_eq!(ime.marked_range(), Some(0..3));
        assert_eq!(ime.len_utf16(), 3);
    }

    #[test]
    fn char_stroke_matches_a_typed_key() {
        assert_eq!(char_stroke('a'), Some(plain("a", "a")));
        assert_eq!(char_stroke(' '), Some(plain("space", " ")));
        assert_eq!(char_stroke('\t'), None);
        let k = char_stroke('Q').unwrap();
        assert!(k.modifiers.shift);
        assert_eq!(k.key, "q");
    }

    fn ctrl_alt() -> Modifiers {
        Modifiers {
            control: true,
            alt: true,
            ..Default::default()
        }
    }

    /// An AltGr key-down as gpui reports it on Windows: Ctrl+Alt changed
    /// ToUnicode's character.
    fn altgr(key: &str, key_char: &str) -> KeyDownEvent {
        KeyDownEvent {
            prefer_character_input: true,
            ..typed(stroke(key, Some(key_char), ctrl_alt()))
        }
    }

    /// macOS (#1162): an ⌥ key with one printable character is the plain
    /// typed key for that character, Shift kept for an upper-case one, and
    /// ⌥Space's no-break space is a character, not Space.
    #[test]
    fn an_option_character_is_a_plain_typed_key_on_macos() {
        let alt_shift = Modifiers {
            shift: true,
            ..alt()
        };
        for (k, want) in [
            (stroke("g", Some("©"), alt()), plain("©", "©")),
            (stroke("2", Some("™"), alt()), plain("™", "™")),
            (stroke("`", Some("`"), alt()), plain("`", "`")),
            (stroke("2", Some("€"), alt_shift), plain("€", "€")),
            (
                stroke("space", Some("\u{a0}"), alt()),
                plain("\u{a0}", "\u{a0}"),
            ),
        ] {
            assert_eq!(modifier_char(&typed(k.clone()), true), Some(want), "{k}");
        }
        let a_acute = modifier_char(&typed(stroke("y", Some("Á"), alt_shift)), true);
        assert_eq!(a_acute, char_stroke('Á'));
        assert_eq!(a_acute.and_then(|k| k.key_char).as_deref(), Some("Á"));
    }

    #[test]
    fn a_dialog_or_a_project_edit_is_a_text_field() {
        assert!(field_open(true, false));
        assert!(field_open(false, true));
        assert!(!field_open(false, false));
    }

    /// Windows only: `route_key` withholds a plain dead key from `on_key`.
    #[test]
    fn only_windows_withholds_a_plain_dead_key_from_on_key() {
        let d = dead("´", "´", Modifiers::default());
        assert!(!passes_to_on_key(&d, true));
        assert!(passes_to_on_key(&d, false));
        assert!(passes_to_on_key(&altgr("e", "€"), true));
        assert!(passes_to_on_key(&typed(plain("n", "ñ")), true));
        assert!(passes_to_on_key(&dead("4", "~", ctrl_alt()), true));
    }

    /// Windows (#1312): a plain dead key is flagged and bare; nothing with a
    /// modifier, no accent or no flag is.
    #[test]
    fn a_plain_dead_key_is_found_by_its_flag_and_bare_modifiers() {
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert!(plain_dead_key(&dead("´", "´", Modifiers::default())));
        assert!(plain_dead_key(&dead("6", "^", shift)));
        assert!(!plain_dead_key(&dead("g", "g", alt())));
        assert!(!plain_dead_key(&dead("4", "~", ctrl_alt())));
        assert!(!plain_dead_key(&altgr("e", "€")));
        assert!(!plain_dead_key(&typed(plain("n", "ñ"))));
        let mut empty = dead("´", "", Modifiers::default());
        assert!(!plain_dead_key(&empty));
        empty.keystroke.key_char = None;
        assert!(!plain_dead_key(&empty));
    }

    /// Keys whose modifiers made no character stay as they are (#1162):
    /// ⌥ arrows, ⌥⌫ and ⌥Enter; ⌃⌥ and ⌘⌥ chords on macOS; and every key
    /// off macOS, Windows' AltGr included (its WM_CHAR types it).
    #[test]
    fn keys_whose_modifiers_made_no_character_are_not_text() {
        let fn_alt = Modifiers {
            function: true,
            ..alt()
        };
        let cmd_alt = Modifiers {
            platform: true,
            ..alt()
        };
        let mac = [
            stroke("left", None, fn_alt),
            stroke("backspace", Some("\u{8}"), alt()),
            stroke("enter", Some("\r"), alt()),
            stroke("g", Some("©"), ctrl_alt()),
            stroke("g", Some("©"), cmd_alt),
            stroke("g", Some("©"), fn_alt),
            stroke("g", Some("g"), Modifiers::default()),
            stroke("e", Some("´x"), alt()),
            stroke("e", None, alt()),
        ];
        for k in &mac {
            assert_eq!(modifier_char(&typed(k.clone()), true), None, "{k}");
        }
        for ev in [
            typed(stroke("g", Some("©"), alt())),
            typed(stroke("g", Some("g"), alt())),
            altgr("e", "€"),
        ] {
            assert_eq!(modifier_char(&ev, false), None, "{}", ev.keystroke);
        }
    }

    /// A dead key's key-down as gpui reports it: ToUnicode was negative.
    fn dead(key: &str, key_char: &str, modifiers: Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            prefer_character_input: true,
            ..typed(stroke(key, Some(key_char), modifiers))
        }
    }

    fn t0() -> Instant {
        Instant::now()
    }

    fn ms(at: Instant, n: u64) -> Instant {
        at + Duration::from_millis(n)
    }

    /// A recording [`KeyHost`]: what reaches `on_key`, in order. A key with
    /// printable text and no Ctrl, Alt or Win is its text ("[shift-space]"
    /// for Shift+Space), any other is "[mods-key]", and a key taken while
    /// KeyTips are up is "kt:key". F10 toggles KeyTips, as `on_key` does;
    /// Enter closes the dialog.
    struct Rec {
        focused: bool,
        keytips: bool,
        dialog: bool,
        log: Vec<String>,
    }

    impl Default for Rec {
        fn default() -> Self {
            Rec {
                focused: true,
                keytips: false,
                dialog: false,
                log: Vec::new(),
            }
        }
    }

    fn label(k: &Keystroke) -> String {
        let m = &k.modifiers;
        let text = k
            .key_char
            .as_deref()
            .filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
        match text {
            Some(t) if !m.control && !m.alt && !m.platform && !(m.shift && k.key == "space") => {
                t.to_string()
            }
            _ => {
                let mut s = String::from("[");
                for (on, name) in [
                    (m.control, "ctrl-"),
                    (m.alt, "alt-"),
                    (m.platform, "win-"),
                    (m.shift, "shift-"),
                ] {
                    if on {
                        s.push_str(name);
                    }
                }
                s.push_str(&k.key);
                s.push(']');
                s
            }
        }
    }

    impl KeyHost for Rec {
        fn focused(&mut self) -> bool {
            self.focused
        }

        fn takes_text(&mut self) -> bool {
            takes_text(self.dialog, self.keytips, false)
        }

        fn text_field_open(&mut self) -> bool {
            self.dialog
        }

        fn on_key(&mut self, ev: &KeyDownEvent) {
            let k = &ev.keystroke;
            let entry = if self.keytips && k.key != "f10" {
                format!("kt:{}", k.key)
            } else {
                label(k)
            };
            self.log.push(entry);
            match k.key.as_str() {
                "f10" => self.keytips = !self.keytips,
                "enter" => self.dialog = false,
                _ => {}
            }
        }

        fn tab(&mut self, shift: bool) {
            self.log
                .push(if shift { "[shift-tab]" } else { "[tab]" }.into());
        }
    }

    /// The Windows queue and an editable surface with the root focused.
    fn win() -> (WmChars, Rec) {
        (WmChars::new(true), Rec::default())
    }

    /// The same off Windows: nothing is withheld, typed keys owe their
    /// text for the harness's WM_CHAR twins.
    fn lin() -> (WmChars, Rec) {
        (WmChars::new(false), Rec::default())
    }

    /// What reached the host since the last call.
    fn took(r: &mut Rec) -> Vec<String> {
        std::mem::take(&mut r.log)
    }

    fn keys(q: &mut WmChars, r: &mut Rec, at: Instant, evs: &[KeyDownEvent]) {
        for ev in evs {
            q.key_down(ev, at, r);
        }
    }

    fn chars(q: &mut WmChars, r: &mut Rec, at: Instant, texts: &[&str]) {
        for t in texts {
            q.char_message(t, at, r);
        }
    }

    fn key(c: &str) -> KeyDownEvent {
        typed(plain(c, c))
    }

    fn cmd(name: &str) -> KeyDownEvent {
        typed(stroke(name, None, Modifiers::default()))
    }

    fn after_wait(at: Instant) -> Instant {
        at + WM_CHAR_WAIT + Duration::from_millis(1)
    }

    #[test]
    fn plain_and_altgr_text_keys_are_withheld_others_are_not() {
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        let win_altgr = KeyDownEvent {
            keystroke: stroke(
                "e",
                Some("€"),
                Modifiers {
                    platform: true,
                    ..ctrl_alt()
                },
            ),
            ..altgr("e", "€")
        };
        assert_eq!(withheld(&key("a")), Some(Withheld::Plain));
        assert_eq!(
            withheld(&typed(stroke("a", Some("A"), shift))),
            Some(Withheld::Plain)
        );
        assert_eq!(withheld(&altgr("e", "€")), Some(Withheld::AltGr));
        assert_eq!(withheld(&dead("4", "~", ctrl_alt())), Some(Withheld::AltGr));
        for ev in [
            cmd("enter"),
            typed(stroke("a", Some("\u{1}"), ctrl)),
            typed(stroke("c", Some("c"), ctrl)),
            typed(stroke("g", Some("g"), alt())),
            typed(stroke("e", Some("e"), ctrl_alt())),
            win_altgr,
            dead("´", "´", Modifiers::default()),
        ] {
            assert_eq!(withheld(&ev), None, "{}", ev.keystroke);
        }
    }

    /// r1-p1 (#1277): AltGr never reaches `on_key` as Ctrl+Alt, so it runs
    /// no Ctrl command (Polish AltGr+C "ć" is not Copy) and is not dropped:
    /// its WM_CHAR types the character once.
    #[test]
    fn altgr_types_its_character_and_no_ctrl_alt_key() {
        let (mut q, mut r) = win();
        let at = t0();
        for (k, c) in [("e", "€"), ("c", "ć"), ("s", "ś"), ("q", "@")] {
            q.key_down(&altgr(k, c), at, &mut r);
            assert!(took(&mut r).is_empty(), "{c}: withheld");
            q.char_message(c, at, &mut r);
            assert_eq!(took(&mut r), [c]);
        }
        assert_eq!(q.owed(), 0);
        assert!(!q.holding());
    }

    /// r3-m1 (#1277): a packet "ï" among typed keys, every WM_CHAR after
    /// the last key-down (gpui's input drain): "naïve", not "naveï".
    #[test]
    fn packet_text_keeps_its_place_among_typed_keys() {
        let (mut q, mut r) = win();
        let at = t0();
        keys(
            &mut q,
            &mut r,
            at,
            &[key("n"), key("a"), key("v"), key("e")],
        );
        assert!(took(&mut r).is_empty());
        chars(&mut q, &mut r, ms(at, 5), &["n", "a", "ï", "v", "e"]);
        assert_eq!(took(&mut r), ["n", "a", "ï", "v", "e"]);
        assert_eq!(q.owed(), 0);
    }

    /// Commands wait behind the text typed before them (#1277): Backspace
    /// after "ab", and Enter after an AltGr "@" in a dialog, which the "@"
    /// reaches before Enter closes it.
    #[test]
    fn commands_keep_their_place_among_late_characters() {
        let (mut q, mut r) = win();
        let at = t0();
        keys(
            &mut q,
            &mut r,
            at,
            &[key("a"), key("b"), cmd("backspace"), key("c")],
        );
        assert!(took(&mut r).is_empty());
        chars(&mut q, &mut r, ms(at, 5), &["a", "b", "c"]);
        assert_eq!(took(&mut r), ["a", "b", "[backspace]", "c"]);
        r.dialog = true;
        keys(&mut q, &mut r, at, &[altgr("q", "@"), cmd("enter")]);
        assert!(took(&mut r).is_empty());
        assert!(r.dialog);
        chars(&mut q, &mut r, ms(at, 5), &["@"]);
        assert_eq!(took(&mut r), ["@", "[enter]"]);
        assert!(!r.dialog);
    }

    /// Tab and Shift+Tab, which gpui hands the root as actions, wait too:
    /// auto-type "a", Tab, "b" puts "a" in the first field.
    #[test]
    fn tab_waits_behind_the_text_before_it() {
        let (mut q, mut r) = win();
        let at = t0();
        q.key_down(&key("a"), at, &mut r);
        q.tab(false, at, &mut r);
        q.key_down(&key("b"), at, &mut r);
        q.tab(true, at, &mut r);
        assert!(took(&mut r).is_empty());
        chars(&mut q, &mut r, ms(at, 5), &["a", "b"]);
        assert_eq!(took(&mut r), ["a", "[tab]", "b", "[shift-tab]"]);
        // With nothing held, a Tab runs at once.
        q.tab(false, at, &mut r);
        assert_eq!(took(&mut r), ["[tab]"]);
    }

    /// r4-m1.2 (#1277): an AltGr dead key's accent, which never comes, never
    /// swallows a later packet's "~": the packet is typed once. Its next
    /// letter's composed text drops it, and Space after it types "~" once.
    #[test]
    fn an_altgr_dead_key_never_swallows_a_character() {
        let (mut q, mut r) = win();
        let at = t0();
        let tilde = dead("4", "~", ctrl_alt());
        q.key_down(&tilde, at, &mut r);
        chars(&mut q, &mut r, ms(at, 10_000), &["~"]);
        assert_eq!(took(&mut r), ["~"]);
        assert_eq!(q.owed(), 0);
        keys(&mut q, &mut r, at, &[tilde.clone(), typed(plain("a", "ã"))]);
        chars(&mut q, &mut r, at, &["ã"]);
        assert_eq!(took(&mut r), ["ã"]);
        assert_eq!(q.owed(), 0);
        keys(&mut q, &mut r, at, &[tilde, typed(plain("space", "~"))]);
        chars(&mut q, &mut r, at, &["~"]);
        assert_eq!(took(&mut r), ["~"]);
        assert_eq!(q.owed(), 0);
        chars(&mut q, &mut r, at, &["~"]);
        assert_eq!(took(&mut r), ["~"], "a packet after them types");
    }

    /// The same edge on a key `on_key` took (KeyTips up): its AltGr debt is
    /// dropped once [`WM_CHAR_WAIT`] passes, so a later packet's "~" types.
    #[test]
    fn a_typed_altgr_debt_lapses_after_the_wait() {
        let (mut q, mut r) = win();
        let at = t0();
        r.keytips = true;
        q.key_down(&dead("4", "~", ctrl_alt()), at, &mut r);
        assert_eq!(took(&mut r), ["kt:4"]);
        r.keytips = false;
        assert_eq!(q.owed(), 1);
        chars(&mut q, &mut r, after_wait(at), &["~"]);
        assert_eq!(took(&mut r), ["~"]);
        assert_eq!(q.owed(), 0);
    }

    /// r4-m1.3 (#1277): no count cap, so 5000 key-downs queued ahead of
    /// their WM_CHARs are each typed once, in order, and off Windows 5000
    /// debts are each paid once.
    #[test]
    fn five_thousand_queued_keys_are_each_typed_once() {
        let letters: Vec<String> = (0..5000)
            .map(|i| char::from(b'a' + (i % 26) as u8).to_string())
            .collect();
        let evs: Vec<KeyDownEvent> = letters.iter().map(|c| key(c)).collect();
        let texts: Vec<&str> = letters.iter().map(String::as_str).collect();
        let at = t0();
        let (mut q, mut r) = win();
        keys(&mut q, &mut r, at, &evs);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 5000);
        chars(&mut q, &mut r, ms(at, 5), &texts);
        assert_eq!(took(&mut r), letters);
        assert_eq!(q.owed(), 0);
        let (mut q, mut r) = lin();
        keys(&mut q, &mut r, at, &evs);
        assert_eq!(took(&mut r), letters);
        assert_eq!(q.owed(), 5000);
        chars(&mut q, &mut r, ms(at, 5), &texts);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 0);
    }

    /// A withheld key whose WM_CHAR does not come is replayed at
    /// [`WM_CHAR_WAIT`], the commands behind it after it, and a WM_CHAR that
    /// comes later still is dropped. Behind an AltGr dead key a command runs
    /// then too.
    #[test]
    fn a_missing_wm_char_holds_for_the_wait_at_most() {
        let (mut q, mut r) = win();
        let at = t0();
        keys(&mut q, &mut r, at, &[key("a"), cmd("backspace")]);
        assert_eq!(q.next_lapse(at), Some(WM_CHAR_WAIT));
        q.release_lapsed(ms(at, 100), &mut r);
        assert!(took(&mut r).is_empty());
        q.release_lapsed(after_wait(at), &mut r);
        assert_eq!(took(&mut r), ["a", "[backspace]"]);
        assert!(!q.holding());
        assert_eq!(q.next_lapse(at), None);
        chars(&mut q, &mut r, after_wait(at), &["a"]);
        assert!(took(&mut r).is_empty());
        keys(
            &mut q,
            &mut r,
            at,
            &[dead("4", "~", ctrl_alt()), cmd("left")],
        );
        assert!(took(&mut r).is_empty());
        q.release_lapsed(after_wait(at), &mut r);
        assert_eq!(took(&mut r), ["[left]"]);
    }

    /// A replayed key is the key-down itself: Shift+Space keeps its Shift.
    #[test]
    fn a_withheld_key_is_replayed_unchanged() {
        let (mut q, mut r) = win();
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let at = t0();
        q.key_down(&typed(stroke("space", Some(" "), shift)), at, &mut r);
        chars(&mut q, &mut r, at, &[" "]);
        assert_eq!(took(&mut r), ["[shift-space]"]);
    }

    /// Under KeyTips letters are commands: `on_key` takes them at key-down
    /// and their WM_CHAR types nothing. A key behind F10 is routed after
    /// F10 ran, so it is a KeyTip too.
    #[test]
    fn under_keytips_letters_go_to_on_key_at_once() {
        let (mut q, mut r) = win();
        let at = t0();
        keys(&mut q, &mut r, at, &[cmd("f10"), key("h")]);
        assert_eq!(took(&mut r), ["[f10]", "kt:h"]);
        chars(&mut q, &mut r, at, &["h"]);
        assert!(took(&mut r).is_empty());
        q.key_down(&cmd("f10"), at, &mut r);
        assert_eq!(took(&mut r), ["[f10]"]);
        keys(&mut q, &mut r, at, &[key("a"), cmd("f10"), key("h")]);
        assert!(took(&mut r).is_empty());
        chars(&mut q, &mut r, at, &["a", "h"]);
        assert_eq!(took(&mut r), ["a", "[f10]", "kt:h"]);
        assert_eq!(q.owed(), 0);
    }

    /// Without the root's focus gpui registers no handler, so no WM_CHAR
    /// comes: the key goes to `on_key` and owes nothing, and the next key
    /// typed with the focus is typed once, not twice.
    #[test]
    fn a_key_without_the_focus_owes_nothing() {
        let (mut q, mut r) = win();
        let at = t0();
        r.focused = false;
        q.key_down(&key("x"), at, &mut r);
        assert_eq!(took(&mut r), ["x"]);
        assert_eq!(q.owed(), 0);
        r.focused = true;
        q.key_down(&key("a"), at, &mut r);
        chars(&mut q, &mut r, at, &["a"]);
        q.release_lapsed(after_wait(at), &mut r);
        assert_eq!(took(&mut r), ["a"]);
    }

    /// A sure debt whose WM_CHAR was lost stops blocking the keys after it
    /// once [`WM_CHAR_WAIT`] has passed.
    #[test]
    fn a_lost_wm_char_blocks_nothing_past_the_wait() {
        let (mut q, mut r) = win();
        let at = t0();
        r.keytips = true;
        q.key_down(&key("x"), at, &mut r);
        r.keytips = false;
        assert_eq!(took(&mut r), ["kt:x"]);
        let later = after_wait(at);
        q.key_down(&key("a"), later, &mut r);
        chars(&mut q, &mut r, later, &["a"]);
        q.release_lapsed(after_wait(later), &mut r);
        assert_eq!(took(&mut r), ["a"]);
    }

    /// A mouse-down releases the held keys first, in order, an AltGr key's
    /// character typed then too ("user@x", then a click on OK); their
    /// WM_CHARs are then dropped.
    #[test]
    fn a_flush_replays_the_held_keys() {
        let (mut q, mut r) = win();
        let at = t0();
        keys(
            &mut q,
            &mut r,
            at,
            &[
                key("a"),
                cmd("backspace"),
                key("b"),
                altgr("q", "@"),
                key("c"),
            ],
        );
        q.flush(at, &mut r);
        assert_eq!(took(&mut r), ["a", "[backspace]", "b", "@", "c"]);
        assert!(!q.holding());
        chars(&mut q, &mut r, at, &["a", "b", "@", "c"]);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 0);
        // Under a dialog that closed before the flush, an AltGr key types
        // nothing, as its WM_CHAR would.
        r.dialog = true;
        q.key_down(&altgr("q", "@"), at, &mut r);
        r.dialog = false;
        q.flush(at, &mut r);
        chars(&mut q, &mut r, at, &["@"]);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 0);
    }

    /// A packet whose character a later key also owes, sent before the keys
    /// (packet "b", keys A and B, every WM_CHAR late), is typed in its place:
    /// "bab", each once. The first "b" is not taken as B's, which would make
    /// A's WM_CHAR look lost (#1277 r1 i2).
    #[test]
    fn a_packet_like_a_later_key_is_typed_in_its_place() {
        let (mut q, mut r) = win();
        let at = t0();
        keys(&mut q, &mut r, at, &[key("a"), key("b")]);
        chars(&mut q, &mut r, ms(at, 5), &["b", "a", "b"]);
        q.release_lapsed(after_wait(after_wait(at)), &mut r);
        assert_eq!(took(&mut r), ["b", "a", "b"]);
        assert_eq!(q.owed(), 0);
    }

    /// A withheld key whose character comes once the app no longer takes
    /// text is dropped, as packet text is: a letter is no KeyTip command.
    #[test]
    fn a_character_after_text_stopped_being_taken_is_dropped() {
        let (mut q, mut r) = win();
        let at = t0();
        q.key_down(&key("a"), at, &mut r);
        r.keytips = true;
        chars(&mut q, &mut r, at, &["a", "z"]);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 0);
    }

    /// Off Windows nothing is withheld: every key reaches `on_key` at once.
    #[test]
    fn off_windows_keys_go_to_on_key_at_once() {
        let (mut q, mut r) = lin();
        let at = t0();
        keys(
            &mut q,
            &mut r,
            at,
            &[key("a"), altgr("e", "€"), cmd("enter")],
        );
        assert_eq!(took(&mut r), ["a", "[ctrl-alt-e]", "[enter]"]);
        assert!(!q.holding());
        q.tab(false, at, &mut r);
        assert_eq!(took(&mut r), ["[tab]"]);
    }

    /// Under a text field (#1162, #1312) an AltGr key types while one is
    /// open, and nothing once it closed before its WM_CHAR came; a dead key
    /// under it gives way to the next letter's composed text.
    #[test]
    fn altgr_under_a_text_field_types_only_while_it_is_open() {
        let (mut q, mut r) = win();
        let at = t0();
        r.dialog = true;
        q.key_down(&altgr("q", "@"), at, &mut r);
        chars(&mut q, &mut r, at, &["@"]);
        assert_eq!(took(&mut r), ["@"]);
        keys(&mut q, &mut r, at, &[dead("4", "~", ctrl_alt()), key("ñ")]);
        chars(&mut q, &mut r, at, &["ñ"]);
        assert_eq!(took(&mut r), ["ñ"]);
        assert_eq!(q.owed(), 0);
        q.key_down(&altgr("q", "@"), at, &mut r);
        q.release_lapsed(after_wait(at), &mut r);
        r.dialog = false;
        chars(&mut q, &mut r, after_wait(at), &["@"]);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 0);
    }

    /// Keys that are not withheld keep their debts under a dialog: plain
    /// Alt (WM_SYSCHAR), Ctrl+Alt that changed nothing and a Win chord reach
    /// `on_key` at once, and their WM_CHAR is not typed again.
    #[test]
    fn other_keys_under_a_dialog_keep_their_debts() {
        let win_altgr = KeyDownEvent {
            keystroke: stroke(
                "e",
                Some("€"),
                Modifiers {
                    platform: true,
                    ..ctrl_alt()
                },
            ),
            ..altgr("e", "€")
        };
        for (ev, wm_char) in [
            (typed(stroke("g", Some("g"), alt())), "g"),
            (dead("g", "g", alt()), "g"),
            (typed(stroke("e", Some("€"), ctrl_alt())), "€"),
            (win_altgr, "€"),
        ] {
            let (mut q, mut r) = win();
            r.dialog = true;
            q.key_down(&ev, t0(), &mut r);
            assert_eq!(took(&mut r).len(), 1, "{}", ev.keystroke);
            chars(&mut q, &mut r, t0(), &[wm_char]);
            if ev.keystroke.modifiers.alt && !ev.keystroke.modifiers.control {
                // No WM_CHAR was owed: the character is a packet's.
                assert_eq!(took(&mut r), [wm_char], "{}", ev.keystroke);
            } else {
                assert!(took(&mut r).is_empty(), "{}", ev.keystroke);
            }
        }
    }

    /// Windows (#1139): a typed key owes its WM_CHAR, so the handler drops
    /// it, and a Unicode packet, which has no key-down, is typed.
    #[test]
    fn a_typed_keys_wm_char_is_dropped_and_packet_text_typed() {
        let (mut q, mut r) = lin();
        let at = t0();
        q.key_down(&key("a"), at, &mut r);
        assert_eq!(took(&mut r), ["a"]);
        chars(&mut q, &mut r, ms(at, 1), &["a"]);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 0);
        // SendInput's KEYEVENTF_UNICODE: WM_CHAR alone.
        chars(&mut q, &mut r, ms(at, 2), &["Z", "é"]);
        assert_eq!(took(&mut r), ["Z", "é"]);
        // A key's twin, then a packet.
        q.key_down(&key("b"), ms(at, 4), &mut r);
        chars(&mut q, &mut r, ms(at, 5), &["b", "é"]);
        assert_eq!(took(&mut r), ["b", "é"]);
    }

    /// A WM_CHAR held behind a stall still finds a sure debt, up to
    /// `WM_CHAR_KEEP`.
    #[test]
    fn a_wm_char_after_a_long_stall_is_still_dropped() {
        let (mut q, mut r) = lin();
        let at = t0();
        q.key_down(&key("a"), at, &mut r);
        took(&mut r);
        chars(&mut q, &mut r, ms(at, 1500), &["a"]);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 0);
    }

    /// The review's "éb": a packet é queued before a typed B, both
    /// key-downs handled before either WM_CHAR. The packet's é pays no debt
    /// (none is "é"), so it types, and B's WM_CHAR pays B's. A packet "e"
    /// ahead of typed T, H, E is typed, and the keys are not doubled.
    #[test]
    fn a_packet_before_a_typed_key_pays_no_debt() {
        let (mut q, mut r) = lin();
        let at = t0();
        q.key_down(&key("b"), at, &mut r);
        took(&mut r);
        chars(&mut q, &mut r, at, &["é", "b"]);
        assert_eq!(took(&mut r), ["é"]);
        keys(&mut q, &mut r, at, &[key("t"), key("h"), key("e")]);
        took(&mut r);
        chars(&mut q, &mut r, at, &["e", "t", "h", "e"]);
        assert_eq!(took(&mut r), ["e"]);
        assert_eq!(q.owed(), 0);
    }

    /// An AltGr dead key then a letter it does not combine with: the
    /// letter's debt "~x" is sure to be paid, so "~" pays it rather than
    /// the dead key's, and "x" is not typed twice. The dead key's debt goes
    /// at [`WM_CHAR_WAIT`].
    #[test]
    fn an_altgr_dead_key_pair_prefers_the_sure_debt() {
        let (mut q, mut r) = lin();
        let at = t0();
        let tilde = dead("4", "~", ctrl_alt());
        assert!(may_not_come(&tilde));
        keys(&mut q, &mut r, at, &[tilde, typed(plain("x", "~x"))]);
        took(&mut r);
        chars(&mut q, &mut r, at, &["~", "x"]);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 1);
        chars(&mut q, &mut r, after_wait(at), &["~"]);
        assert_eq!(took(&mut r), ["~"]);
    }

    /// A packet "b" queued ahead of a typed AltGr € and a typed B: the €
    /// debt, whose WM_CHAR is still coming, is not dropped by B's match, so
    /// only the packet's "b" is typed (in the place of B's twin).
    #[test]
    fn a_match_drops_no_older_typed_altgr_debt() {
        let (mut q, mut r) = lin();
        let at = t0();
        keys(&mut q, &mut r, at, &[dead("e", "€", ctrl_alt()), key("b")]);
        took(&mut r);
        chars(&mut q, &mut r, at, &["b", "€", "b"]);
        assert_eq!(took(&mut r), ["b"]);
        assert_eq!(q.owed(), 0);
        // Withheld on Windows, the text comes out in the order sent.
        let (mut q, mut r) = win();
        keys(&mut q, &mut r, at, &[altgr("e", "€"), key("b")]);
        chars(&mut q, &mut r, at, &["b", "€", "b"]);
        assert_eq!(took(&mut r), ["b", "€", "b"]);
        assert_eq!(q.owed(), 0);
    }

    /// A plain dead key reaches nothing and owes nothing on Windows; the
    /// composed letter owes "é". A packet after it types in full.
    #[test]
    fn a_dead_key_owes_nothing_and_its_letter_the_composed_text() {
        let (mut q, mut r) = win();
        let at = t0();
        let acute = dead("´", "´", Modifiers::default());
        assert_eq!(owed_text(&acute), None);
        q.key_down(&acute, at, &mut r);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 0);
        assert!(!q.holding());
        q.key_down(&typed(plain("e", "é")), ms(at, 100), &mut r);
        chars(&mut q, &mut r, ms(at, 101), &["é", "é"]);
        assert_eq!(took(&mut r), ["é", "é"]);
        // "´" then X: one key-down, two WM_CHARs, typed once.
        keys(&mut q, &mut r, at, &[acute, typed(plain("x", "´x"))]);
        assert_eq!(q.owed(), 2);
        chars(&mut q, &mut r, at, &["´", "x"]);
        assert_eq!(took(&mut r), ["´x"]);
        assert_eq!(q.owed(), 0);
        // A WM_CHAR with more than is owed types the rest.
        q.key_down(&key("a"), at, &mut r);
        chars(&mut q, &mut r, at, &["aé"]);
        assert_eq!(took(&mut r), ["a", "é"]);
    }

    /// gpui joins a surrogate pair into one WM_CHAR call: one character,
    /// one typed key; and a key typing one owes one character.
    #[test]
    fn a_non_bmp_packet_is_one_typed_key() {
        let (mut q, mut r) = win();
        let at = t0();
        chars(&mut q, &mut r, at, &["😀"]);
        assert_eq!(took(&mut r), ["😀"]);
        q.key_down(&key("😀"), at, &mut r);
        assert_eq!(q.owed(), 1);
        chars(&mut q, &mut r, at, &["😀", "x"]);
        assert_eq!(took(&mut r), ["😀", "x"]);
    }

    /// Keys that make no WM_CHAR owe nothing, so a packet after them types:
    /// Enter and arrows have no printable text, Alt+letter makes
    /// WM_SYSCHAR, and a Ctrl chord's text is a control character. A Win
    /// chord the shell lets through owes its letter.
    #[test]
    fn keys_without_a_wm_char_owe_nothing() {
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        let win_mod = Modifiers {
            platform: true,
            ..Default::default()
        };
        for k in [
            stroke("enter", Some("\r"), Modifiers::default()),
            stroke("left", None, Modifiers::default()),
            stroke("f", Some("f"), alt()),
            stroke("a", Some("\u{1}"), ctrl),
        ] {
            assert_eq!(owed_text(&typed(k.clone())), None, "{k}");
            let (mut q, mut r) = win();
            q.key_down(&typed(k.clone()), t0(), &mut r);
            assert_eq!(took(&mut r).len(), 1, "{k}");
            chars(&mut q, &mut r, t0(), &["f"]);
            assert_eq!(took(&mut r), ["f"], "{k}");
        }
        let win_y = typed(stroke("y", Some("y"), win_mod));
        assert_eq!(owed_text(&win_y).as_deref(), Some("y"));
        let (mut q, mut r) = win();
        q.key_down(&win_y, t0(), &mut r);
        assert_eq!(took(&mut r), ["[win-y]"]);
        chars(&mut q, &mut r, t0(), &["y"]);
        assert!(took(&mut r).is_empty());
    }

    /// gpui forwards an IME message with no text as an empty commit: it
    /// types nothing and pays off nothing. Control characters never type.
    #[test]
    fn an_empty_wm_char_changes_nothing() {
        let (mut q, mut r) = win();
        q.key_down(&key("a"), t0(), &mut r);
        chars(&mut q, &mut r, t0(), &[""]);
        assert!(took(&mut r).is_empty());
        assert_eq!(q.owed(), 1);
        chars(&mut q, &mut r, t0(), &["a", "\u{8}"]);
        assert_eq!(took(&mut r), ["a"]);
    }
    /// The handler turns gpui's input method on only on macOS: the Windows
    /// IME stays off as it was before the handler was registered there.
    #[test]
    fn only_macos_accepts_text_input() {
        assert!(accepts_text_input(true, true));
        assert!(!accepts_text_input(true, false));
        assert!(!accepts_text_input(false, true));
        assert!(!accepts_text_input(false, false));
    }

    /// The harness's real input stands in for the window procedure: on
    /// Windows a key `on_key` typed gets its WM_CHAR twin.
    #[test]
    fn real_input_gets_a_wm_char_twin_only_on_windows() {
        let key = PlatformInput::KeyDown(typed(plain("a", "a")));
        assert_eq!(char_message_text(&key, true).as_deref(), Some("a"));
        assert_eq!(char_message_text(&key, false), None);
        let alt_f = PlatformInput::KeyDown(typed(stroke("f", Some("f"), alt())));
        assert_eq!(char_message_text(&alt_f, true), None);
        let euro = PlatformInput::KeyDown(typed(stroke("e", Some("€"), ctrl_alt())));
        assert_eq!(char_message_text(&euro, true).as_deref(), Some("€"));
    }
}
