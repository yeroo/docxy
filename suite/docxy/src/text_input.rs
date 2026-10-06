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
//! The handler is registered on macOS only: Windows sends every WM_CHAR to a
//! registered handler and Linux every unhandled printable key, and `on_key`
//! has already typed those. Off macOS the root also leaves every key
//! propagating, as it always has: on Windows a handled key-down is never
//! translated, which would lose WM_CHAR and the system's Alt+F4 and Alt+Space.
//! Everything else here builds everywhere, and [`route`] takes the platform
//! as an argument, so the macOS rules are tested on every CI leg.

use std::ops::Range;

use gpui::{
    Bounds, Context, EntityInputHandler, KeyDownEvent, Keystroke, Modifiers, Pixels, PlatformInput,
    Point, UTF16Selection, Window,
};

use crate::Docxy;

/// Whether this build registers the input handler and defers keys to it.
pub(crate) const MACOS: bool = cfg!(target_os = "macos");

/// Where the window root sends a key-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// Leave it to AppKit's input context, which answers through the input
    /// handler: `insertText:` or `setMarkedText:`.
    Defer,
    /// `on_key` takes it and the platform hears it was handled, so it never
    /// also reaches the input handler as text.
    AppStop,
    /// `on_key` sees it and it still propagates: a Cmd chord, which AppKit
    /// offers to the menu bar and the system (Cmd+Q, Cmd+H, Cmd+`) when the
    /// window leaves it unhandled, and every key off macOS.
    AppPropagate,
}

/// The route for one key-down. On macOS, printable text with no Ctrl, Cmd or
/// Fn goes to AppKit (the keys gpui itself would let an input method see)
/// while the app `takes_text` (see [`takes_text`]); a held key's repeats stay
/// with `on_key`, as they always have. Off macOS every key goes to `on_key`
/// and propagates, exactly as before #1072.
pub(crate) fn route(keystroke: &Keystroke, is_held: bool, macos: bool, takes_text: bool) -> Route {
    let m = &keystroke.modifiers;
    if !macos || m.platform {
        return Route::AppPropagate;
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

/// The composition in progress and the key AppKit has been handed.
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
                self.on_key(ev, window, cx);
                cx.stop_propagation();
            }
            Route::AppPropagate => {
                self.ime.forget_key();
                self.on_key(ev, window, cx);
            }
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
        let keys = self.ime.commit(text);
        self.type_committed(keys, window, cx);
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
    // context before the root only while this is true.
    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.takes_text()
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

    #[test]
    fn cmd_chords_keep_propagating_for_the_menu_bar() {
        let cmd = Modifiers {
            platform: true,
            ..Default::default()
        };
        for macos in [true, false] {
            assert_eq!(
                route(&stroke("c", Some("c"), cmd), false, macos, true),
                Route::AppPropagate
            );
            assert_eq!(
                route(&stroke("q", None, cmd), false, macos, true),
                Route::AppPropagate
            );
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
}
