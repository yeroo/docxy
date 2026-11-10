//! Find in the embedded document view (#1887): the box the view draws at
//! its top right (Ctrl+F, ⌘F on macOS), what a key does while it is open,
//! and where its hits paint. The matching and stepping are the document
//! tab's (`Editor::find_visible`, case-insensitive, and `find_step_in`), so
//! the two cannot drift; this module holds the box's own state and
//! decisions over plain values, tested without a window.

use super::*;
use docxcore::editor::FoundMatch;
use gpui::Keystroke;

/// The box's top edge, from the view's top.
pub(crate) const BOX_TOP: f32 = 8.;
/// The box's height.
pub(crate) const BOX_H: f32 = 30.;
/// The box's right edge, from the view's right: clear of the 12 px
/// vertical scroll bar strip.
pub(crate) const BOX_RIGHT: f32 = 8. + crate::doc_scrollbar::BAR_PX;
/// The band at the view's top the open box covers: a match or the caret is
/// only in view below it, so a revealed one is never under the box.
pub(crate) const BOX_BAND: f32 = BOX_TOP + BOX_H + 4.;

/// A hit's highlight: amber at 35%, over the glyphs, which reads with
/// black ink on the white page and light ink on a dark one. The current
/// match is the selection instead, as in a document tab.
pub(crate) fn hit_color() -> Hsla {
    rgba(0xFFB0_0059).into()
}

/// The box's state. Its field is a one-line field kept as a plain string
/// (`field_edit`, as the Help tab's search box), with a whole-field
/// selection.
#[derive(Debug, Default)]
pub(crate) struct FindState {
    pub open: bool,
    pub query: String,
    pub caret: usize,
    pub sel: bool,
    /// The field has the keys. A press in the document gives them back to
    /// it; the box stays open with its hits.
    pub field_focused: bool,
    /// The current match and its index, as the document tab's find bar keeps
    /// it: stepped from while the editor is still on it.
    pub cur: Option<(usize, FoundMatch)>,
}

impl FindState {
    /// The field takes the keys with its whole text selected (Ctrl+F, open
    /// or again), so typing replaces the query.
    pub(crate) fn focus_select_all(&mut self) {
        self.field_focused = true;
        self.edit(FieldOp::SelectAll.op());
    }

    /// A press on the field: the keys, with the caret at the end.
    pub(crate) fn focus_at_end(&mut self) {
        self.field_focused = true;
        self.sel = false;
        self.caret = self.query.chars().count();
    }

    /// One field edit; whether the query changed.
    pub(crate) fn edit(&mut self, op: crate::field_edit::Op) -> bool {
        let before = self.query.clone();
        crate::field_edit::apply(&mut self.query, &mut self.caret, &mut self.sel, op);
        self.query != before
    }

    /// The selected field text a Ctrl+C or Ctrl+X takes, if any.
    pub(crate) fn selected_text(&self) -> Option<&str> {
        (self.sel && !self.query.is_empty()).then_some(self.query.as_str())
    }

    /// The current match's index in `matches`, while it is still the match
    /// at that index and the editor is still on it (no press, edit or caret
    /// move since); `None` otherwise.
    pub(crate) fn current(&self, ed: &Editor, matches: &[FoundMatch]) -> Option<usize> {
        let (i, m) = self.cur.as_ref()?;
        (matches.get(*i) == Some(m) && ed.is_at_found(m)).then_some(*i)
    }
}

/// A field edit a key makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldOp {
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    SelectAll,
}

impl FieldOp {
    pub(crate) fn op(self) -> crate::field_edit::Op<'static> {
        use crate::field_edit::Op;
        match self {
            FieldOp::Backspace => Op::Backspace,
            FieldOp::Delete => Op::Delete,
            FieldOp::Left => Op::Left,
            FieldOp::Right => Op::Right,
            FieldOp::Home => Op::Home,
            FieldOp::End => Op::End,
            FieldOp::SelectAll => Op::SelectAll,
        }
    }
}

/// What a key does with Find.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FindKey {
    /// Not Find's: the view's keys as without it (the editing map, the
    /// read-only map, or on to the suite).
    Pass,
    /// Ctrl+F: open the box, or with it open give its field the keys with
    /// the text selected.
    Open,
    /// Escape: close the box; the selection and the query stay.
    Close,
    /// Enter / F3 (`back`: with Shift): the next or previous match.
    Step {
        back: bool,
    },
    Field(FieldOp),
    Type(String),
    Copy,
    Cut,
    Paste,
    /// The read-only view's scroll and zoom keys (Up/Down, Page Up/Down,
    /// Ctrl+Home/End, Ctrl+= / - / 0), never the editing map.
    View,
    /// Taken and dropped: an unmodified key that is not the field's, so it
    /// never reaches the document.
    Swallow,
    /// Any other chord: on to the suite as the read-only view sends it,
    /// never to the editing map (Ctrl+W, Ctrl+S, Ctrl+Tab, Alt, F-keys).
    Bubble,
}

/// What key `ks` does with Find, the box open or not and its field having
/// the keys or not. `macos` picks ⌘ as the Ctrl+F chord and ⌥ as text.
pub(crate) fn find_key(ks: &Keystroke, macos: bool, open: bool, field_focused: bool) -> FindKey {
    let m = ks.modifiers;
    let key = ks.key.as_str();
    let secondary = if macos { m.platform } else { m.control };
    let other_mod = if macos { m.control } else { m.platform };
    if key == "f" && secondary && !other_mod && !m.shift && !m.alt && !m.function {
        return FindKey::Open;
    }
    if !open {
        return FindKey::Pass;
    }
    let chord = m.control || m.platform || m.alt;
    match key {
        "escape" if !chord => return FindKey::Close,
        "f3" if !chord => return FindKey::Step { back: m.shift },
        _ => {}
    }
    if !field_focused {
        return FindKey::Pass;
    }
    if key == "enter" && !chord {
        return FindKey::Step { back: m.shift };
    }
    if matches!(key, "shift" | "control" | "alt" | "platform" | "function") {
        return FindKey::Bubble;
    }
    if let Some(text) = super::edit::typed_text(ks, macos) {
        return FindKey::Type(text.to_string());
    }
    let ctrl = m.control || m.platform;
    // Ctrl+Alt is AltGr off macOS: a key that made no text is no chord.
    if ctrl && !m.alt {
        return match key {
            "a" if !m.shift => FindKey::Field(FieldOp::SelectAll),
            "c" if !m.shift => FindKey::Copy,
            "x" if !m.shift => FindKey::Cut,
            "v" if !m.shift => FindKey::Paste,
            // Ctrl+Backspace / Delete delete a character, as the document
            // find bar's field does.
            "backspace" => FindKey::Field(FieldOp::Backspace),
            "delete" => FindKey::Field(FieldOp::Delete),
            "home" | "end" if !m.shift => FindKey::View,
            "=" | "+" => FindKey::View,
            "-" | "0" if !m.shift => FindKey::View,
            _ => FindKey::Bubble,
        };
    }
    if m.alt || m.function || is_function_key(key) {
        return FindKey::Bubble;
    }
    // Shift moves as the plain key: the field has no partial selection.
    match key {
        "backspace" => FindKey::Field(FieldOp::Backspace),
        "delete" => FindKey::Field(FieldOp::Delete),
        "left" => FindKey::Field(FieldOp::Left),
        "right" => FindKey::Field(FieldOp::Right),
        "home" => FindKey::Field(FieldOp::Home),
        "end" => FindKey::Field(FieldOp::End),
        "up" | "down" | "pageup" | "pagedown" if !m.shift => FindKey::View,
        _ => FindKey::Swallow,
    }
}

/// `f1` .. `f24`.
fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The byte index of char `ci` in `text`, clamped to its end: a run drawn
/// transformed (all caps, `ß` as `SS`) can hold other chars than the model,
/// and a hit then paints short, never out of the layout.
pub(crate) fn char_byte(text: &str, ci: usize) -> usize {
    text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b)
}

/// One painted piece of a paragraph: its first char, its length in chars,
/// its bounds, and whether it is a unit (a tab, a field, a picture) that
/// paints whole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HitPiece {
    pub off: usize,
    pub len: usize,
    pub bounds: Bounds<Pixels>,
    pub unit: bool,
}

/// The quads that highlight chars `start..end` of a paragraph over its
/// painted `pieces`: per piece the match overlaps, from `x_at(piece, char
/// in piece)` (its text's x there, `None` when unknown) to the overlap's
/// end; a unit paints its whole bounds. A collapsed match (a read-only one
/// with no width) paints nothing, nor does a piece that drew nothing.
pub(crate) fn hit_quads(
    pieces: &[HitPiece],
    start: usize,
    end: usize,
    x_at: impl Fn(usize, usize) -> Option<Pixels>,
) -> Vec<Bounds<Pixels>> {
    let mut out = Vec::new();
    if start >= end {
        return out;
    }
    for (i, p) in pieces.iter().enumerate() {
        let b = p.bounds;
        if b.size.height <= px(0.) || p.len == 0 {
            continue;
        }
        let (a, z) = (start.max(p.off), end.min(p.off + p.len));
        if a >= z {
            continue;
        }
        if p.unit {
            out.push(b);
            continue;
        }
        let clamp = |x: Pixels| x.max(b.left()).min(b.right());
        let x0 = clamp(x_at(i, a - p.off).unwrap_or(b.left()));
        let x1 = clamp(x_at(i, z - p.off).unwrap_or(b.right()));
        // A piece wrapped inside its own layout gives an end before its
        // start: the whole piece, then. Both clamped to one edge: nothing.
        let (x0, x1) = match x1.partial_cmp(&x0) {
            Some(std::cmp::Ordering::Greater) => (x0, x1),
            Some(std::cmp::Ordering::Less) => (b.left(), b.right()),
            _ => continue,
        };
        out.push(Bounds::from_corners(
            point(x0, b.top()),
            point(x1, b.bottom()),
        ));
    }
    out
}

/// The part of the view a match or the caret is in view in: clear of the
/// horizontal bar (#2080) and, with the bar shown, of the vertical one, and
/// below the open box's band.
pub(crate) fn find_view(
    view: Bounds<Pixels>,
    hbar_shown: bool,
    vbar_shown: bool,
    box_open: bool,
) -> Bounds<Pixels> {
    let mut v = crate::doc_scrollbar::clear_of_hbar(view, hbar_shown);
    if vbar_shown {
        v.size.width = (v.size.width - px(crate::doc_scrollbar::BAR_PX)).max(px(0.));
    }
    if box_open {
        let band = px(BOX_BAND).min(v.size.height);
        v.origin.y += band;
        v.size.height -= band;
    }
    v
}

/// The change of the scroll area's x offset that brings `piece` into
/// `view`, as [`super::reveal_dy`] does in y: a Find step shows its match
/// sideways too (a match in a table wider than the view, #1895).
pub(crate) fn reveal_dx(view: Bounds<Pixels>, piece: Bounds<Pixels>) -> Pixels {
    if piece.left() < view.left() || piece.size.width > view.size.width {
        view.left() - piece.left()
    } else if piece.right() > view.right() {
        view.right() - piece.right()
    } else {
        px(0.)
    }
}

/// The box's count text: "2 of 11", "11 results" off a match, "No
/// results", nothing for an empty query.
pub(crate) fn count_text(query: &str, count: usize, current: Option<usize>) -> String {
    if query.is_empty() {
        String::new()
    } else if count == 0 {
        "No results".into()
    } else if let Some(i) = current {
        format!("{} of {count}", i + 1)
    } else if count == 1 {
        "1 result".into()
    } else {
        format!("{count} results")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use docxcore::editor::Caret;
    use docxcore::model::{Inline, Paragraph, Run};

    fn ks(s: &str) -> Keystroke {
        let mut k = Keystroke::parse(s).unwrap();
        let plain = !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform;
        if plain && k.key.chars().count() == 1 {
            let c = if k.modifiers.shift {
                k.key.to_uppercase()
            } else {
                k.key.clone()
            };
            k.key_char = Some(c);
        } else if plain && k.key == "space" {
            k.key_char = Some(" ".into());
        }
        k
    }

    /// The box open with its field having the keys, off macOS.
    fn focused(s: &str) -> FindKey {
        find_key(&ks(s), false, true, true)
    }

    #[test]
    fn ctrl_f_opens_or_refocuses_and_cmd_f_on_macos() {
        for (open, field) in [(false, false), (true, false), (true, true)] {
            assert_eq!(find_key(&ks("ctrl-f"), false, open, field), FindKey::Open);
        }
        assert_eq!(find_key(&ks("cmd-f"), true, false, false), FindKey::Open);
        // Ctrl+F on macOS and ⌘F elsewhere are not Find's; nor is
        // Ctrl+Shift+F or Ctrl+Alt+F.
        assert_eq!(find_key(&ks("ctrl-f"), true, false, false), FindKey::Pass);
        assert_eq!(find_key(&ks("cmd-f"), false, false, false), FindKey::Pass);
        assert_eq!(
            find_key(&ks("ctrl-shift-f"), false, false, false),
            FindKey::Pass
        );
        assert_eq!(
            find_key(&ks("ctrl-alt-f"), false, false, false),
            FindKey::Pass
        );
    }

    #[test]
    fn with_the_box_closed_every_other_key_is_the_views() {
        for k in ["escape", "f3", "shift-f3", "enter", "a", "ctrl-w", "tab"] {
            assert_eq!(find_key(&ks(k), false, false, false), FindKey::Pass, "{k}");
        }
    }

    #[test]
    fn escape_and_f3_work_whatever_the_field_focus() {
        for field in [false, true] {
            let at = |k: &str| find_key(&ks(k), false, true, field);
            assert_eq!(at("escape"), FindKey::Close);
            assert_eq!(at("f3"), FindKey::Step { back: false });
            assert_eq!(at("shift-f3"), FindKey::Step { back: true });
        }
        // The field gave the keys back: Enter and typing are the document's.
        for k in ["enter", "a", "backspace", "ctrl-b", "up"] {
            assert_eq!(find_key(&ks(k), false, true, false), FindKey::Pass, "{k}");
        }
    }

    #[test]
    fn enter_steps_and_shift_enter_steps_back() {
        assert_eq!(focused("enter"), FindKey::Step { back: false });
        assert_eq!(focused("shift-enter"), FindKey::Step { back: true });
    }

    #[test]
    fn the_field_types_and_edits_and_shift_moves_as_the_plain_key() {
        assert_eq!(focused("a"), FindKey::Type("a".into()));
        assert_eq!(focused("shift-l"), FindKey::Type("L".into()));
        assert_eq!(focused("space"), FindKey::Type(" ".into()));
        assert_eq!(focused("backspace"), FindKey::Field(FieldOp::Backspace));
        assert_eq!(focused("delete"), FindKey::Field(FieldOp::Delete));
        assert_eq!(
            focused("ctrl-backspace"),
            FindKey::Field(FieldOp::Backspace)
        );
        assert_eq!(focused("ctrl-delete"), FindKey::Field(FieldOp::Delete));
        assert_eq!(focused("left"), FindKey::Field(FieldOp::Left));
        assert_eq!(focused("shift-left"), FindKey::Field(FieldOp::Left));
        assert_eq!(focused("right"), FindKey::Field(FieldOp::Right));
        assert_eq!(focused("shift-home"), FindKey::Field(FieldOp::Home));
        assert_eq!(focused("end"), FindKey::Field(FieldOp::End));
        assert_eq!(focused("ctrl-a"), FindKey::Field(FieldOp::SelectAll));
        assert_eq!(focused("ctrl-c"), FindKey::Copy);
        assert_eq!(focused("ctrl-x"), FindKey::Cut);
        assert_eq!(focused("ctrl-v"), FindKey::Paste);
    }

    #[test]
    fn altgr_types_and_a_plain_alt_is_never_text_off_macos() {
        let mut k = Keystroke::parse("ctrl-alt-q").unwrap();
        k.key_char = Some("@".into());
        assert_eq!(find_key(&k, false, true, true), FindKey::Type("@".into()));
        let mut k = Keystroke::parse("alt-2").unwrap();
        k.key_char = Some("™".into());
        assert_eq!(find_key(&k, false, true, true), FindKey::Bubble);
        // On macOS ⌥ makes the character.
        assert_eq!(find_key(&k, true, true, true), FindKey::Type("™".into()));
    }

    #[test]
    fn the_scroll_and_zoom_keys_go_to_the_view_map() {
        for k in [
            "up",
            "down",
            "pageup",
            "pagedown",
            "ctrl-home",
            "ctrl-end",
            "ctrl-=",
            "ctrl-+",
            "ctrl--",
            "ctrl-0",
        ] {
            assert_eq!(focused(k), FindKey::View, "{k}");
        }
    }

    #[test]
    fn other_chords_bubble_and_never_reach_the_document_while_the_field_has_the_keys() {
        // The suite's own keys go on (Ctrl+W closes the tab), and the
        // document's editing chords never reach the editing map.
        for k in [
            "ctrl-w",
            "ctrl-s",
            "ctrl-n",
            "ctrl-tab",
            "ctrl-shift-tab",
            "ctrl-f6",
            "ctrl-f12",
            "ctrl-b",
            "ctrl-z",
            "ctrl-y",
            "ctrl-i",
            "ctrl-u",
            "ctrl-m",
            "ctrl-shift-s",
            "alt",
            "alt-h",
            "f1",
            "f10",
            "shift-f12",
            "shift",
        ] {
            assert_eq!(focused(k), FindKey::Bubble, "{k}");
        }
        // An unmodified key that is not the field's is dropped.
        for k in ["tab", "insert", "shift-up", "shift-pagedown"] {
            assert_eq!(focused(k), FindKey::Swallow, "{k}");
        }
    }

    #[test]
    fn opening_selects_the_query_and_a_press_puts_the_caret_at_its_end() {
        let mut f = FindState {
            query: "Line 7".into(),
            ..FindState::default()
        };
        f.focus_select_all();
        assert!(f.field_focused && f.sel);
        assert_eq!(f.selected_text(), Some("Line 7"));
        // Typing replaces the selected query.
        assert!(f.edit(crate::field_edit::Op::Insert("B")));
        assert_eq!((f.query.as_str(), f.caret, f.sel), ("B", 1, false));
        assert_eq!(f.selected_text(), None);
        f.query = "Beta".into();
        f.focus_at_end();
        assert_eq!((f.caret, f.sel), (4, false));
        // A move changes no query.
        assert!(!f.edit(crate::field_edit::Op::Left));
    }

    fn b(x: f32, w: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(10.)), size(px(w), px(20.)))
    }

    /// Pieces "Line " (0..5 at x 100, 10 px a char) and "7" (5..6 at x 150).
    fn line7() -> Vec<HitPiece> {
        vec![
            HitPiece {
                off: 0,
                len: 5,
                bounds: b(100., 50.),
                unit: false,
            },
            HitPiece {
                off: 5,
                len: 1,
                bounds: b(150., 10.),
                unit: false,
            },
        ]
    }

    fn x_at(pieces: &[HitPiece]) -> impl Fn(usize, usize) -> Option<Pixels> + '_ {
        move |i, c| Some(pieces[i].bounds.left() + px(10. * c as f32))
    }

    #[test]
    fn a_hit_inside_one_piece_paints_its_chars_only() {
        let p = line7();
        let q = hit_quads(&p, 1, 3, x_at(&p));
        assert_eq!(
            q,
            [Bounds::from_corners(
                point(px(110.), px(10.)),
                point(px(130.), px(30.))
            )]
        );
    }

    #[test]
    fn a_hit_across_two_pieces_paints_one_quad_in_each() {
        let p = line7();
        let q = hit_quads(&p, 0, 6, x_at(&p));
        assert_eq!(q, [b(100., 50.), b(150., 10.)]);
        // A hit in the second piece only leaves the first alone.
        let q = hit_quads(&p, 5, 6, x_at(&p));
        assert_eq!(q, [b(150., 10.)]);
    }

    #[test]
    fn a_unit_paints_whole_and_a_collapsed_hit_or_an_empty_piece_nothing() {
        let unit = [HitPiece {
            off: 3,
            len: 1,
            bounds: b(40., 30.),
            unit: true,
        }];
        assert_eq!(hit_quads(&unit, 2, 4, |_, _| None), [b(40., 30.)]);
        let p = line7();
        assert!(hit_quads(&p, 2, 2, x_at(&p)).is_empty());
        let empty = [HitPiece {
            off: 0,
            len: 1,
            bounds: Bounds::default(),
            unit: true,
        }];
        assert!(hit_quads(&empty, 0, 1, |_, _| None).is_empty());
    }

    #[test]
    fn a_layout_shorter_than_the_model_paints_a_short_hit_never_past_it() {
        // "straße" drawn as one piece whose layout holds "STRASSE": the
        // char→byte map clamps at the text's end.
        assert_eq!(char_byte("abc", 1), 1);
        assert_eq!(char_byte("äbc", 1), 2);
        assert_eq!(char_byte("abc", 9), 3);
        let p = [HitPiece {
            off: 0,
            len: 6,
            bounds: b(0., 60.),
            unit: false,
        }];
        // An x past the layout clamps to the piece's right edge: a short
        // hit inside the piece; one wholly past it paints nothing.
        let at = |_: usize, c: usize| Some(px(20. * c as f32));
        assert_eq!(
            hit_quads(&p, 2, 6, at),
            [Bounds::from_corners(
                point(px(40.), px(10.)),
                point(px(60.), px(30.))
            )]
        );
        assert!(hit_quads(&p, 4, 6, at).is_empty());
    }

    #[test]
    fn the_view_for_a_match_clears_the_bars_and_the_open_box() {
        let view = Bounds::new(point(px(0.), px(100.)), size(px(600.), px(400.)));
        assert_eq!(find_view(view, false, false, false), view);
        let v = find_view(view, true, true, true);
        assert_eq!(v.top(), px(100. + BOX_BAND));
        assert_eq!(v.bottom(), px(500. - crate::doc_scrollbar::BAR_PX));
        assert_eq!(v.right(), px(600. - crate::doc_scrollbar::BAR_PX));
    }

    #[test]
    fn a_match_off_to_the_right_scrolls_sideways_to_its_edge() {
        let view = Bounds::new(point(px(0.), px(0.)), size(px(500.), px(400.)));
        let right = Bounds::new(point(px(520.), px(10.)), size(px(40.), px(20.)));
        assert_eq!(reveal_dx(view, right), px(-60.));
        let left = Bounds::new(point(px(-30.), px(10.)), size(px(40.), px(20.)));
        assert_eq!(reveal_dx(view, left), px(30.));
        let inside = Bounds::new(point(px(30.), px(10.)), size(px(40.), px(20.)));
        assert_eq!(reveal_dx(view, inside), px(0.));
    }

    #[test]
    fn the_count_reads_the_current_match_or_how_many() {
        assert_eq!(count_text("", 0, None), "");
        assert_eq!(count_text("x", 0, None), "No results");
        assert_eq!(count_text("x", 11, Some(1)), "2 of 11");
        assert_eq!(count_text("x", 11, None), "11 results");
        assert_eq!(count_text("x", 1, None), "1 result");
    }

    fn lines() -> Editor {
        let body = (1..=80)
            .map(|i| {
                Block::Paragraph(Paragraph {
                    content: vec![Inline::Run(Run {
                        text: format!("Line {i}"),
                        ..Run::default()
                    })],
                    ..Paragraph::default()
                })
            })
            .collect();
        let mut ed = Editor::new(Document { body });
        ed.caret = Caret::at(vec![0], 0);
        ed
    }

    #[test]
    fn stepping_wraps_through_the_matches_and_a_moved_caret_steps_from_itself() {
        let mut ed = lines();
        let q = "line 7";
        assert_eq!(ed.find_visible(q, false).len(), 11);
        let cur = find_step_in(&mut ed, q, false, None, false, true);
        assert_eq!(
            cur.as_ref().map(|c| (c.0, c.1.path.clone())),
            Some((0, vec![6]))
        );
        let cur = find_step_in(&mut ed, q, false, cur, false, false);
        assert_eq!(
            cur.as_ref().map(|c| (c.0, c.1.path.clone())),
            Some((1, vec![69]))
        );
        let back = find_step_in(&mut ed, q, false, cur, true, false);
        let back = find_step_in(&mut ed, q, false, back, true, false);
        // Back from the first wraps to the last, Line 79.
        assert_eq!(
            back.as_ref().map(|c| (c.0, c.1.path.clone())),
            Some((10, vec![78]))
        );
        let next = find_step_in(&mut ed, q, false, back.clone(), false, false);
        assert_eq!(next.as_ref().map(|c| c.0), Some(0));
        let f = FindState {
            cur: next,
            ..FindState::default()
        };
        let matches = ed.find_visible(q, false);
        assert_eq!(f.current(&ed, &matches), Some(0));
        // A press elsewhere: the editor left the match, so it is no longer
        // current.
        ed.caret = Caret::at(vec![40], 0);
        ed.clear_selection();
        assert_eq!(f.current(&ed, &matches), None);
    }
}
