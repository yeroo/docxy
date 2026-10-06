//! Word's highlighting mode (#623): the pointer becomes a highlighter. Pure
//! state, so the transitions are unit-tested without a window.

use std::time::{Duration, Instant};

/// A second Text highlight click within this long of the one that started the
/// mode is a double-click and latches it.
pub const LATCH_WINDOW: Duration = Duration::from_millis(500);

/// The colour used when nothing else has been chosen.
pub const DEFAULT_COLOUR: &str = "yellow";

/// What the Text highlight button should do after a click.
#[derive(Debug, PartialEq, Eq)]
pub enum ButtonClick {
    /// Open the swatch picker (a selection exists, no mode on).
    OpenPicker,
    /// The mode started (collapsed caret).
    Started,
    /// The mode is now latched across drags (double-click).
    Latched,
    /// The mode ended (a later click on the button).
    Ended,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlMode {
    pub colour: String,
    pub latched: bool,
    /// The tab the mode belongs to. The app ends the mode on a tab switch, a
    /// close or open; this also keeps a stray mode from acting on another tab.
    pub tab: usize,
    started: Instant,
}

impl HlMode {
    pub fn start(colour: &str, tab: usize, now: Instant) -> Self {
        HlMode {
            colour: colour.to_string(),
            latched: false,
            tab,
            started: now,
        }
    }

    /// A click on the Text highlight button. `mode` is the current state,
    /// `has_selection` whether the document has a non-empty selection.
    pub fn click(
        mode: &mut Option<HlMode>,
        tab: usize,
        has_selection: bool,
        last: &str,
        now: Instant,
    ) -> ButtonClick {
        // A mode left over from another tab is none at all.
        if mode.as_ref().is_some_and(|m| m.tab != tab) {
            *mode = None;
        }
        match mode {
            Some(m) if !m.latched && now.duration_since(m.started) <= LATCH_WINDOW => {
                m.latched = true;
                ButtonClick::Latched
            }
            Some(_) => {
                *mode = None;
                ButtonClick::Ended
            }
            None if has_selection => ButtonClick::OpenPicker,
            None => {
                *mode = Some(HlMode::start(last, tab, now));
                ButtonClick::Started
            }
        }
    }

    /// A drag ended. `selected` says it selected something. Returns the colour
    /// to apply (if any); the mode ends unless latched or nothing was crossed.
    pub fn drag_end(mode: &mut Option<HlMode>, selected: bool) -> Option<String> {
        let m = mode.as_ref()?;
        if !selected {
            return None;
        }
        let colour = m.colour.clone();
        if !m.latched {
            *mode = None;
        }
        Some(colour)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn collapsed_click_starts_and_selection_click_opens_picker() {
        let mut m = None;
        let now = t0();
        assert_eq!(
            HlMode::click(&mut m, 0, true, "yellow", now),
            ButtonClick::OpenPicker
        );
        assert!(m.is_none());
        assert_eq!(
            HlMode::click(&mut m, 0, false, "green", now),
            ButtonClick::Started
        );
        let m = m.unwrap();
        assert_eq!(m.colour, "green");
        assert!(!m.latched);
    }

    #[test]
    fn quick_second_click_latches_and_a_slow_one_ends() {
        let now = t0();
        let mut m = None;
        HlMode::click(&mut m, 0, false, "yellow", now);
        assert_eq!(
            HlMode::click(&mut m, 0, false, "yellow", now + Duration::from_millis(200)),
            ButtonClick::Latched
        );
        assert!(m.as_ref().unwrap().latched);
        // A third click ends it, however fast.
        assert_eq!(
            HlMode::click(&mut m, 0, false, "yellow", now + Duration::from_millis(300)),
            ButtonClick::Ended
        );
        assert!(m.is_none());
        HlMode::click(&mut m, 0, false, "yellow", now);
        assert_eq!(
            HlMode::click(&mut m, 0, false, "yellow", now + Duration::from_millis(900)),
            ButtonClick::Ended
        );
        assert!(m.is_none());
    }

    #[test]
    fn a_mode_from_another_tab_does_not_latch_or_end() {
        let now = t0();
        let mut m = Some(HlMode::start("pink", 1, now));
        // On tab 0 the click starts a fresh mode with the last colour.
        assert_eq!(
            HlMode::click(&mut m, 0, false, "yellow", now),
            ButtonClick::Started
        );
        let m = m.unwrap();
        assert_eq!((m.tab, m.colour.as_str(), m.latched), (0, "yellow", false));
    }

    #[test]
    fn drag_applies_and_ends_unless_latched_or_empty() {
        let now = t0();
        let mut m = Some(HlMode::start("pink", 0, now));
        assert_eq!(HlMode::drag_end(&mut m, false), None);
        assert!(m.is_some(), "a drag that crossed nothing keeps the mode");
        assert_eq!(HlMode::drag_end(&mut m, true).as_deref(), Some("pink"));
        assert!(m.is_none());
        let mut m = Some(HlMode::start("pink", 0, now));
        m.as_mut().unwrap().latched = true;
        assert_eq!(HlMode::drag_end(&mut m, true).as_deref(), Some("pink"));
        assert!(m.is_some(), "latched survives the drag");
        assert_eq!(HlMode::drag_end(&mut None, true), None);
    }
}
