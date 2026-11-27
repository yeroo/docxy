//! A library caller of `docxy_suite` can add HTML beside a copy's text and
//! hand its last copy over at quit (#2303). This file is a crate of its own
//! against `docxy_suite`, so a `pub(crate)` slip in either entry fails to
//! compile here.
//!
//! It also runs them, with a text no clipboard holds, so both return without
//! effect. Headless they do nothing. In CI (`xvfb-run`, no `WAYLAND_DISPLAY`)
//! they go through the real X11 path: the suite's clipboard opens on the xvfb
//! display, the read-back finds no owner, and there is nothing to hand over.
//! On Wayland, with no gpui client in this process, they do nothing. On a
//! developer's X11 desktop a local run reads the real `CLIPBOARD` once and
//! writes nothing, since the text cannot match: the one exception to the
//! unit tests never opening `DISPLAY`'s clipboard.

use docxy_suite::rich_clip;

#[test]
fn the_html_write_and_handover_are_public_and_return() {
    let text = "docxy #2303: a text no clipboard holds \u{2603} 7f3c9e1a";
    rich_clip::write_html("<b>x</b>".to_string(), text);
    rich_clip::hand_over();
}
