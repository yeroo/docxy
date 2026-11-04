//! Tearing a tab off the strip into a window of its own, as a browser tab
//! does: drag a tab chip away from the strip (below it, above it, or out of
//! the window) and let go. The chip's drag is the one that reorders tabs
//! (#545); a release over another chip still reorders, a release away from
//! the strip moves the tab through New Window (#587), the new window's
//! top-left near the release. With one tab there is nothing to tear off: the
//! window says so, as New Window does.
//!
//! gpui runs a drop handler only over its element, so the release that tears
//! has no target. The chip's `on_drag_move` (which gpui calls for every move
//! of a drag, wherever the pointer is) notes the drag here, and the window's
//! mouse-up listener (`Docxy::tab_tear_release`) decides.

use crate::TabDrag;
use gpui::{Bounds, Pixels, Point, SharedString, Size, Window, WindowId, px};
use std::cell::RefCell;

/// How far from the strip's row a release must land to tear (px).
pub(crate) const MARGIN: f32 = 36.;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tear {
    pub window: WindowId,
    pub ix: usize,
    pub len: usize,
    pub title: SharedString,
    pub at: Point<Pixels>,
}

thread_local! {
    static TEAR: RefCell<Option<Tear>> = const { RefCell::new(None) };
}

/// A tab drag moved to `at` (window pixels) in `window`.
pub(crate) fn moved(d: &TabDrag, at: Point<Pixels>, window: &Window) {
    let t = Tear {
        window: window.window_handle().window_id(),
        ix: d.ix,
        len: d.len,
        title: d.title.clone(),
        at,
    };
    TEAR.with(|c| *c.borrow_mut() = Some(t));
}

/// The drag noted for `window`, taken: every release ends it.
pub(crate) fn take(window: &Window) -> Option<Tear> {
    let id = window.window_handle().window_id();
    TEAR.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().is_some_and(|t| t.window == id) {
            c.take()
        } else {
            None
        }
    })
}

/// Whether a release at `at` tears: out of the window, or more than
/// [`MARGIN`] above or below the dragged chip's row.
pub(crate) fn torn(at: Point<Pixels>, chip: Bounds<Pixels>, viewport: Size<Pixels>) -> bool {
    let outside = at.x < px(0.) || at.y < px(0.) || at.x > viewport.width || at.y > viewport.height;
    outside || at.y > chip.bottom() + px(MARGIN) || at.y < chip.top() - px(MARGIN)
}

/// Where the torn-off window's top-left goes: the release point on screen,
/// less a little so the pointer sits over the new window's tab strip.
pub(crate) fn origin(window_origin: Point<Pixels>, at: Point<Pixels>) -> Point<Pixels> {
    Point {
        x: window_origin.x + at.x - px(120.),
        y: window_origin.y + at.y - px(16.),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn chip() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(200.), px(6.)),
            size: size(px(140.), px(26.)),
        }
    }

    #[test]
    fn a_release_on_the_strip_row_does_not_tear() {
        let vp = size(px(1180.), px(800.));
        assert!(!torn(point(px(400.), px(20.)), chip(), vp));
        assert!(!torn(point(px(900.), px(60.)), chip(), vp));
    }

    #[test]
    fn a_release_away_from_the_strip_or_outside_the_window_tears() {
        let vp = size(px(1180.), px(800.));
        assert!(torn(point(px(400.), px(300.)), chip(), vp));
        assert!(torn(point(px(-20.), px(20.)), chip(), vp));
        assert!(torn(point(px(400.), px(-5.)), chip(), vp));
        assert!(torn(point(px(1300.), px(20.)), chip(), vp));
    }

    #[test]
    fn the_new_window_opens_under_the_pointer() {
        let o = origin(point(px(100.), px(50.)), point(px(400.), px(300.)));
        assert_eq!(o, point(px(380.), px(334.)));
    }
}
