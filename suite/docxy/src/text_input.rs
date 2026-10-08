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
//! WM_CHAR, which reaches nothing but the handler. A real key's key-down has
//! already been typed by `on_key`, and its WM_CHAR follows it, so the root
//! records the text each key-down owes ([`owed_text`]) and the handler drops
//! WM_CHAR text that matches it, in order, typing only what matches nothing.
//! The key's text is what gpui's own ToUnicode gave, which leaves the
//! dead-key state alone for TranslateMessage: a plain dead key owes nothing
//! (it makes WM_DEADCHAR) and the letter after it owes "é", which the one
//! WM_CHAR "é" matches. The WM_CHAR may come late: TranslateMessage posts it,
//! and a queue drained of input alone (gpui does that under load, and
//! automation may queue a key's down and up at once) hands over the key-up
//! and later key-downs first. So a debt outlives its key's key-up and waits
//! for its text up to [`WM_CHAR_KEEP`] (60 s). A match pays only its own
//! debt and drops none: the character may be a packet's, queued ahead of the
//! WM_CHARs older debts wait for, and a WM_CHAR does not say which it is. So
//! an AltGr dead key's accent, which never comes, waits out the keep time.
//! A character pays the oldest debt that starts with it, preferring one sure
//! to be paid. A packet's text that no debt starts with is typed: matched by
//! text, not counted. A packet whose character a pending debt starts with
//! pays that debt, and the key's own WM_CHAR is then typed in its place, so
//! the text comes out the same. Packet text queued behind typed keys under
//! load may land after their text. The
//! handler never accepts text input there ([`accepts_text_input`]), so the
//! Windows IME stays off as it was.
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

/// At most this many debts are kept; the oldest go first (#1139). Hygiene
/// too: a WM_CHAR queued behind more key-downs than this is typed again.
pub(crate) const WM_CHAR_MAX_DEBTS: usize = 4096;

/// The WM_CHAR text a Windows key-down `on_key` has typed will send (#1139):
/// its printable text, or none. Alt alone makes WM_SYSCHAR, not WM_CHAR
/// (AltGr is Ctrl+Alt, which does). A Win chord the shell lets through is
/// translated like any key, so it owes its text. A dead key makes
/// WM_DEADCHAR, so it owes nothing: gpui flags a key whose ToUnicode is dead
/// as preferring character input, and with no Ctrl, Alt or Win held nothing
/// else sets that flag. Under AltGr the flag marks every character the
/// modifiers change too, so an AltGr dead key owes its accent ([`may_not_come`])
/// until it lapses. "´" then X owes "´x", which comes as two WM_CHARs.
pub(crate) fn owed_text(ev: &KeyDownEvent) -> Option<String> {
    let m = &ev.keystroke.modifiers;
    if m.alt && !m.control {
        return None;
    }
    if ev.prefer_character_input && !m.control && !m.alt && !m.platform {
        return None;
    }
    let keystroke = &ev.keystroke;
    keystroke
        .key_char
        .clone()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
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

/// The text of the WM_CHAR Windows posts for a key event `on_key` typed: what
/// the harness's queued real input sends the handler when `windows`, since it
/// never passes through the window procedure. `None` for any other event, and
/// for a key that owes no WM_CHAR ([`owed_text`]).
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

/// What one Windows key-down `on_key` typed still owes in WM_CHAR (#1139).
#[derive(Clone, Debug)]
struct Debt {
    /// The text not yet matched.
    text: String,
    /// When the key was typed, for [`WM_CHAR_KEEP`].
    at: Instant,
    /// [`may_not_come`]: a newer debt's match drops it.
    may_not_come: bool,
}

/// The composition in progress and the key AppKit has been handed, and on
/// Windows the WM_CHAR characters a typed key still owes.
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
    /// Windows: the WM_CHAR text each key-down `on_key` typed still owes,
    /// oldest first, and when it was typed (#1139).
    owed: VecDeque<Debt>,
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

    /// Windows: a key-down `on_key` typed at `now`; its WM_CHAR is on the
    /// way, maybe behind the WM_CHARs of keys before it.
    pub(crate) fn key_down(&mut self, ev: &KeyDownEvent, now: Instant) {
        self.lapse(now);
        if let Some(text) = owed_text(ev) {
            if self.owed.len() == WM_CHAR_MAX_DEBTS {
                self.owed.pop_front();
            }
            self.owed.push_back(Debt {
                text,
                at: now,
                may_not_come: may_not_come(ev),
            });
        }
    }

    /// Drops the debts kept past [`WM_CHAR_KEEP`].
    fn lapse(&mut self, now: Instant) {
        self.owed
            .retain(|d| now.saturating_duration_since(d.at) <= WM_CHAR_KEEP);
    }

    /// Windows' WM_CHAR at `now` (a surrogate pair joined by gpui): the key
    /// events that type what no key-down typed already, one per character.
    /// Each character pays the oldest debt that starts with it, preferring
    /// one sure to be paid over one that [`may_not_come`]. Only that debt
    /// changes, and none is dropped: the character may be a packet's, ahead
    /// of the WM_CHARs older debts still wait for (an older AltGr debt among
    /// them). A character no debt starts with (a packet's) is typed and
    /// leaves the debts as they are. Empty text (an IME message gpui
    /// forwards) changes nothing.
    pub(crate) fn char_message(&mut self, text: &str, now: Instant) -> Vec<KeyDownEvent> {
        self.lapse(now);
        let mut keys = Vec::new();
        for c in text.chars() {
            let owes = |d: &Debt| d.text.starts_with(c);
            let found = self
                .owed
                .iter()
                .position(|d| !d.may_not_come && owes(d))
                .or_else(|| self.owed.iter().position(owes));
            let Some(i) = found else {
                keys.extend(char_stroke(c).map(typed));
                continue;
            };
            let debt = &mut self.owed[i].text;
            debt.drain(..c.len_utf8());
            if debt.is_empty() {
                self.owed.remove(i);
            }
        }
        keys
    }

    /// The WM_CHAR characters typed keys still owe, lapsed or not.
    pub(crate) fn owed(&self) -> usize {
        self.owed.iter().map(|d| d.text.chars().count()).sum()
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
                self.on_key(ev, window, cx);
                // After on_key, which may run long (a save): the debt's
                // keeping time starts once the key is typed.
                self.ime.key_down(ev, Instant::now());
            }
            Route::MenuBar => self.ime.forget_key(),
        }
    }

    /// AppKit's `insertText:` (#1072): see [`ImeState::commit`].
    pub(crate) fn macos_commit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.ime.commit(text);
        self.type_committed(keys, window, cx);
    }

    /// Windows' WM_CHAR (#1139): see [`ImeState::char_message`]. Text is
    /// typed only while the app takes it; under KeyTips or a menu its
    /// letters would be commands.
    pub(crate) fn windows_char(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.ime.char_message(text, Instant::now());
        if !keys.is_empty() && self.takes_text() {
            self.type_committed(keys, window, cx);
        }
    }

    fn takes_text(&self) -> bool {
        takes_text(
            self.active_dialogs().is_some_and(|d| d.is_open()),
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

    /// Windows (#1139): a typed key owes its WM_CHAR, so the handler drops
    /// it, and a Unicode packet, which has no key-down, is typed.
    #[test]
    fn a_typed_keys_wm_char_is_dropped_and_packet_text_typed() {
        let mut ime = ImeState::default();
        let at = t0();
        ime.key_down(&typed(plain("a", "a")), at);
        assert!(ime.char_message("a", ms(at, 1)).is_empty());
        assert_eq!(ime.owed(), 0);
        // SendInput's KEYEVENTF_UNICODE: WM_CHAR alone.
        assert_eq!(key_chars(&ime.char_message("Z", ms(at, 2))), ["Z"]);
        assert_eq!(key_chars(&ime.char_message("é", ms(at, 3))), ["é"]);
        // A key's twin, then a packet.
        ime.key_down(&typed(plain("b", "b")), ms(at, 4));
        assert!(ime.char_message("b", ms(at, 5)).is_empty());
        assert_eq!(key_chars(&ime.char_message("é", ms(at, 6))), ["é"]);
    }

    /// A WM_CHAR held behind a stall still finds its debt, up to
    /// `WM_CHAR_KEEP`.
    #[test]
    fn a_wm_char_after_a_long_stall_is_still_dropped() {
        let mut ime = ImeState::default();
        let at = t0();
        ime.key_down(&typed(plain("a", "a")), at);
        assert!(ime.char_message("a", ms(at, 1500)).is_empty());
        assert_eq!(ime.owed(), 0);
    }

    /// Two key-downs drained before either WM_CHAR: each WM_CHAR pays its
    /// own key's debt, so neither letter types twice.
    #[test]
    fn queued_key_downs_each_owe_their_text() {
        let mut ime = ImeState::default();
        let at = t0();
        ime.key_down(&typed(plain("a", "a")), at);
        ime.key_down(&typed(plain("b", "b")), ms(at, 1));
        assert_eq!(ime.owed(), 2);
        assert!(ime.char_message("a", ms(at, 20)).is_empty());
        assert!(ime.char_message("b", ms(at, 20)).is_empty());
        assert_eq!(key_chars(&ime.char_message("é", ms(at, 21))), ["é"]);
    }

    /// The review's "éb": a packet é queued before a typed B, both
    /// key-downs handled before either WM_CHAR. The packet's é pays no debt
    /// (none is "é"), so it types, and B's WM_CHAR pays B's.
    #[test]
    fn a_packet_before_a_typed_key_pays_no_debt() {
        let mut ime = ImeState::default();
        let at = t0();
        ime.key_down(&typed(plain("b", "b")), at);
        assert_eq!(key_chars(&ime.char_message("é", at)), ["é"]);
        assert!(ime.char_message("b", at).is_empty());
        assert_eq!(ime.owed(), 0);
    }

    /// A packet's character that some typed key also owes pays that key's
    /// debt alone: the older debts' WM_CHARs are still on the way.
    #[test]
    fn a_match_pays_only_its_own_debt() {
        let mut ime = ImeState::default();
        let at = t0();
        for c in ["t", "h", "e"] {
            ime.key_down(&typed(plain(c, c)), at);
        }
        let mut typed_chars = Vec::new();
        for c in ["e", "t", "h", "e"] {
            typed_chars.extend(
                key_chars(&ime.char_message(c, at))
                    .into_iter()
                    .map(str::to_string),
            );
        }
        assert_eq!(typed_chars, ["e"]);
        assert_eq!(ime.owed(), 0);
        // A packet "b" ahead of typed A and B: "b" is typed once, A and B
        // are not doubled.
        ime.key_down(&typed(plain("a", "a")), at);
        ime.key_down(&typed(plain("b", "b")), at);
        let mut typed_chars = Vec::new();
        for c in ["b", "a", "b"] {
            typed_chars.extend(
                key_chars(&ime.char_message(c, at))
                    .into_iter()
                    .map(str::to_string),
            );
        }
        assert_eq!(typed_chars, ["b"]);
        assert_eq!(ime.owed(), 0);
    }

    /// Many key-downs drained before their WM_CHARs, then a long stall:
    /// every WM_CHAR still pays its own key's debt.
    #[test]
    fn a_long_queue_and_a_long_stall_still_pay() {
        let mut ime = ImeState::default();
        let at = t0();
        let letters: Vec<String> = (0..100)
            .map(|i| char::from(b'a' + (i % 26) as u8).to_string())
            .collect();
        for c in &letters {
            ime.key_down(&typed(plain(c, c)), at);
        }
        for c in &letters {
            assert!(ime.char_message(c, at).is_empty(), "{c}");
        }
        assert_eq!(ime.owed(), 0);
        ime.key_down(&typed(plain("a", "a")), at);
        assert!(ime.char_message("a", ms(at, 10_000)).is_empty());
    }

    /// An AltGr dead key then a letter it does not combine with: the
    /// letter's debt "~x" is sure to be paid, so "~" pays it rather than
    /// the dead key's, and "x" is not typed twice.
    #[test]
    fn an_altgr_dead_key_pair_prefers_the_sure_debt() {
        let mut ime = ImeState::default();
        let at = t0();
        let tilde = dead("4", "~", ctrl_alt());
        assert!(may_not_come(&tilde));
        ime.key_down(&tilde, at);
        ime.key_down(&typed(plain("x", "~x")), at);
        assert!(ime.char_message("~", at).is_empty());
        assert!(ime.char_message("x", at).is_empty());
        // The dead key's accent waits out the keep time.
        assert_eq!(ime.owed(), 1);
        let later = at + WM_CHAR_KEEP + Duration::from_millis(1);
        assert_eq!(key_chars(&ime.char_message("~", later)), ["~"]);
    }

    /// A packet "b" queued ahead of an AltGr € and a typed B: no match
    /// drops the € debt, whose WM_CHAR is still coming, so only the
    /// packet's "b" is typed (in the place of B's twin), not "€b€b".
    #[test]
    fn a_match_drops_no_older_altgr_debt() {
        let mut ime = ImeState::default();
        let at = t0();
        ime.key_down(&dead("e", "€", ctrl_alt()), at);
        ime.key_down(&typed(plain("b", "b")), at);
        let mut typed_chars = Vec::new();
        for c in ["b", "€", "b"] {
            typed_chars.extend(
                key_chars(&ime.char_message(c, at))
                    .into_iter()
                    .map(str::to_string),
            );
        }
        assert_eq!(typed_chars, ["b"]);
        assert_eq!(ime.owed(), 0);
    }

    /// A dead key makes WM_DEADCHAR and owes nothing; the composed letter
    /// owes "é". A packet after it types in full, however soon.
    #[test]
    fn a_dead_key_owes_nothing_and_its_letter_the_composed_text() {
        let mut ime = ImeState::default();
        let at = t0();
        let acute = dead("´", "´", Modifiers::default());
        assert_eq!(owed_text(&acute), None);
        ime.key_down(&acute, at);
        assert_eq!(ime.owed(), 0);
        ime.key_down(&typed(plain("e", "é")), ms(at, 100));
        assert!(ime.char_message("é", ms(at, 101)).is_empty());
        assert_eq!(ime.owed(), 0);
        assert_eq!(key_chars(&ime.char_message("é", ms(at, 102))), ["é"]);
        // Typing on after it is typed once.
        for c in ["a", "b", "c"] {
            ime.key_down(&typed(plain(c, c)), ms(at, 103));
            assert!(ime.char_message(c, ms(at, 104)).is_empty(), "{c}");
        }
    }

    /// An AltGr dead key cannot be told from an AltGr character, so it owes
    /// its accent: the composed letter's WM_CHAR pays the letter's debt, and
    /// the accent's, which never comes, waits out the keep time.
    #[test]
    fn an_altgr_dead_keys_debt_waits_out_the_keep_time() {
        let mut ime = ImeState::default();
        let at = t0();
        let tilde = dead("4", "~", ctrl_alt());
        assert_eq!(owed_text(&tilde).as_deref(), Some("~"));
        ime.key_down(&tilde, at);
        ime.key_down(&typed(plain("n", "ñ")), at);
        assert!(ime.char_message("ñ", at).is_empty());
        assert_eq!(ime.owed(), 1);
        let later = at + WM_CHAR_KEEP + Duration::from_millis(1);
        assert_eq!(key_chars(&ime.char_message("~", later)), ["~"]);
    }

    /// An unmatched debt goes after `WM_CHAR_KEEP`, and typing after it is
    /// typed once: "abc" is never "abcc".
    #[test]
    fn a_lapsed_debt_leaves_later_typing_alone() {
        let mut ime = ImeState::default();
        let at = t0();
        let tilde = dead("4", "~", ctrl_alt());
        ime.key_down(&tilde, at);
        let later = at + WM_CHAR_KEEP + Duration::from_millis(1);
        for c in ["a", "b", "c"] {
            ime.key_down(&typed(plain(c, c)), later);
        }
        assert_eq!(ime.owed(), 3);
        for c in ["a", "b", "c"] {
            assert!(ime.char_message(c, later).is_empty(), "{c}");
        }
        assert_eq!(key_chars(&ime.char_message("~", later)), ["~"]);
    }

    /// Debts are bounded where no WM_CHAR ever pays them (Linux, macOS).
    #[test]
    fn debts_are_capped() {
        let mut ime = ImeState::default();
        let at = t0();
        for _ in 0..WM_CHAR_MAX_DEBTS + 10 {
            ime.key_down(&typed(plain("a", "a")), at);
        }
        assert_eq!(ime.owed(), WM_CHAR_MAX_DEBTS);
    }

    /// gpui joins a surrogate pair into one WM_CHAR call: one character,
    /// one typed key.
    #[test]
    fn a_non_bmp_packet_is_one_typed_key() {
        let mut ime = ImeState::default();
        let at = t0();
        let keys = ime.char_message("😀", at);
        assert_eq!(key_chars(&keys), ["😀"]);
        assert_eq!(keys[0].keystroke.key, "😀");
        // And a key typing one owes one character, not two UTF-16 units.
        ime.key_down(&typed(plain("😀", "😀")), at);
        assert_eq!(ime.owed(), 1);
        assert!(ime.char_message("😀", at).is_empty());
        assert_eq!(key_chars(&ime.char_message("x", at)), ["x"]);
    }

    /// Keys that make no WM_CHAR owe nothing, so a packet after them types:
    /// Enter, Tab and arrows have no printable text, Alt+letter makes
    /// WM_SYSCHAR, and a Ctrl chord's text is a control character. A Win
    /// chord the shell lets through owes its letter.
    #[test]
    fn keys_without_a_wm_char_owe_nothing() {
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        let win = Modifiers {
            platform: true,
            ..Default::default()
        };
        for k in [
            stroke("enter", Some("\r"), Modifiers::default()),
            stroke("tab", None, Modifiers::default()),
            stroke("left", None, Modifiers::default()),
            stroke("f", Some("f"), alt()),
            stroke("a", Some("\u{1}"), ctrl),
        ] {
            assert_eq!(owed_text(&typed(k.clone())), None, "{k}");
            let mut ime = ImeState::default();
            ime.key_down(&typed(k.clone()), t0());
            assert_eq!(key_chars(&ime.char_message("f", t0())), ["f"], "{k}");
        }
        let win_y = stroke("y", Some("y"), win);
        assert_eq!(owed_text(&typed(win_y.clone())).as_deref(), Some("y"));
        let mut ime = ImeState::default();
        ime.key_down(&typed(win_y), t0());
        assert!(ime.char_message("y", t0()).is_empty());
    }

    /// AltGr is Ctrl+Alt on Windows and its character comes as WM_CHAR, so
    /// it is typed once, not twice.
    #[test]
    fn an_altgr_character_is_typed_once() {
        let mut ime = ImeState::default();
        let euro = stroke("e", Some("€"), ctrl_alt());
        assert_eq!(owed_text(&typed(euro.clone())).as_deref(), Some("€"));
        ime.key_down(&typed(euro), t0());
        assert!(ime.char_message("€", t0()).is_empty());
        assert_eq!(ime.owed(), 0);
    }

    /// "´" then X sends two WM_CHARs for one key-down, both owed; a WM_CHAR
    /// with more than what is owed types the rest.
    #[test]
    fn a_dead_key_pair_owes_two() {
        let mut ime = ImeState::default();
        let at = t0();
        ime.key_down(&dead("´", "´", Modifiers::default()), at);
        ime.key_down(&typed(plain("x", "´x")), at);
        assert_eq!(ime.owed(), 2);
        assert!(ime.char_message("´", at).is_empty());
        assert!(ime.char_message("x", at).is_empty());
        assert_eq!(ime.owed(), 0);
        assert_eq!(key_chars(&ime.char_message("y", at)), ["y"]);
        ime.key_down(&typed(plain("a", "a")), at);
        assert_eq!(key_chars(&ime.char_message("aé", at)), ["é"]);
    }

    /// gpui forwards an IME message with no text as an empty commit: it
    /// types nothing and pays off nothing.
    #[test]
    fn an_empty_wm_char_changes_nothing() {
        let mut ime = ImeState::default();
        ime.key_down(&typed(plain("a", "a")), t0());
        assert!(ime.char_message("", t0()).is_empty());
        assert_eq!(ime.owed(), 1);
        assert!(ime.char_message("a", t0()).is_empty());
        // Control characters are never typed.
        assert!(ime.char_message("\u{8}", t0()).is_empty());
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
