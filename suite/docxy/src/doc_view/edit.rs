//! The editing mode of the embedded document view (#1947): what a key, a
//! Tab or a host's formatting command does to the view's [`Editor`], and
//! what the caret's formatting reads. It is the document tab's own map
//! (`Docxy::on_key`, `apply_doc_act`, `tab_key` / `shift_tab_key`) without
//! the suite's extras (AutoCorrect, Repeat, fields, breaks, paste), over a
//! plain editor and package, so it is tested without a window.

use super::*;
use docxcore::editor::Clip;
use gpui::Keystroke;

/// What a key did to the editor.
#[derive(Debug)]
pub(crate) enum KeyOutcome {
    /// Not the editing mode's key: the view's own (scroll, zoom) or the suite's.
    Ignored,
    /// Taken without changing the document: a move, a selection, or an edit
    /// with nothing to do (Backspace at the start, Ctrl+Z with no history).
    Moved,
    /// The document changed.
    Changed,
    /// Ctrl+C: the view writes the selection to the clipboard.
    Copy,
    /// Ctrl+X cut this, which the view writes to the clipboard; the document
    /// changed.
    Cut(Clip),
}

impl KeyOutcome {
    /// Whether the document changed.
    pub(crate) fn changed(&self) -> bool {
        matches!(self, KeyOutcome::Changed | KeyOutcome::Cut(_))
    }
}

/// A formatting command a host's ribbon gives an editable view
/// ([`DocView::format`]). Each is one undo step, as the document tab's
/// ribbon command is.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum DocFormat {
    /// Toggle bold over the selection, or for what is typed next at a
    /// collapsed caret (as Ctrl+B).
    Bold,
    Italic,
    Underline,
    /// The selection's font family. Nothing at a collapsed caret, as the
    /// document tab's Font box.
    Font(String),
    /// The selection's size in points, rounded to a half point (1 to 1638).
    /// Nothing at a collapsed caret.
    Size(f32),
    /// The selection's colour as hex `RRGGBB` (a leading `#` is allowed), or
    /// `None` for Automatic.
    Color(Option<String>),
    /// The selection's highlight, a Word highlight name (`yellow`,
    /// `brightGreen`, ...; see `docxcore::agent::HIGHLIGHT_NAMES`), or `None`
    /// to remove it.
    Highlight(Option<String>),
    /// Toggle a bullet list over the selected paragraphs: off when every one
    /// is already in it.
    Bullets,
    /// Toggle a numbered list, as [`DocFormat::Bullets`].
    Numbering,
    /// Increase Indent: a list item one level deeper, another paragraph
    /// half an inch further in.
    Indent,
    /// Decrease Indent.
    Outdent,
    /// The selected paragraphs' alignment.
    Align(Align),
}

/// The formatting at the caret, for a host ribbon's pressed states
/// ([`DocView::caret_format`]): what the document tab's ribbon shows. Over a
/// selection it is the caret end's, never a mixed state; a format toggled at
/// a collapsed caret for what is typed next counts (#854).
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct CaretFormat {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// The font family shown at the caret: the run's own over what its
    /// paragraph, character and table styles give (#1427). `None` only when
    /// neither the run nor its styles set one (the document's default).
    pub font: Option<String>,
    /// The size in points shown at the caret, the run's own over its
    /// styles'; `None` only when neither sets one.
    pub size: Option<f32>,
    /// The colour shown at the caret as hex `RRGGBB`, the run's own over its
    /// styles'; `None` when neither sets one (Automatic).
    pub color: Option<String>,
    /// A Word highlight name.
    pub highlight: Option<String>,
    /// Every selected paragraph is in the bullet list.
    pub bullets: bool,
    /// Every selected paragraph is in the numbered list.
    pub numbering: bool,
    /// The caret paragraph's alignment.
    pub align: Align,
}

/// The top-level blocks an edit at `ed`'s caret and selection can change:
/// those the selection spans and one on each side (Backspace at a
/// paragraph's start merges into the one before it, Delete at its end
/// takes in the next). A block before or after the window moves only by
/// the body's length changing, which [`edit`] compares too.
fn edit_window(ed: &Editor) -> std::ops::Range<usize> {
    let len = ed.doc.body.len();
    let top = |c: &Caret| c.path.first().copied().unwrap_or(0);
    let (a, b) = match &ed.anchor {
        Some(anchor) => (
            top(&ed.caret).min(top(anchor)),
            top(&ed.caret).max(top(anchor)),
        ),
        None => (top(&ed.caret), top(&ed.caret)),
    };
    a.saturating_sub(1).min(len)..(b + 2).min(len)
}

/// Run `f`, an edit at the caret or over the selection, then say whether it
/// changed the document: `Changed` when it did, `Moved` when it ran with
/// nothing to do. It compares the body's length and the blocks of
/// [`edit_window`] before and after: an edit that coalesces into the newest
/// undo step pushes no step, and a step may change nothing (an alignment
/// the paragraph had, a toggle at a collapsed caret), so the undo history
/// cannot tell. A keystroke costs the blocks around the caret, never the
/// document.
fn edit(ed: &mut Editor, f: impl FnOnce(&mut Editor)) -> KeyOutcome {
    let window = edit_window(ed);
    let (len, before) = (ed.doc.body.len(), ed.doc.body[window.clone()].to_vec());
    f(ed);
    let same = ed.doc.body.len() == len && ed.doc.body[window] == before[..];
    if same {
        KeyOutcome::Moved
    } else {
        KeyOutcome::Changed
    }
}

/// Undo (`redo`: Redo) the newest step: `ran` when a step was taken back,
/// `changed` when the document differs after it. A step restores any block,
/// so this one compares the whole body; it runs once per Ctrl+Z, never per
/// typed character. A step that changed nothing (an alignment the
/// paragraph had) undoes with no change, so the host hears of none.
pub(crate) fn history(ed: &mut Editor, redo: bool) -> Formatted {
    let before = ed.doc.body.clone();
    let ran = if redo { ed.redo() } else { ed.undo() };
    Formatted {
        ran,
        changed: ran && ed.doc.body != before,
    }
}

impl From<Formatted> for KeyOutcome {
    fn from(f: Formatted) -> Self {
        if f.changed {
            KeyOutcome::Changed
        } else {
            KeyOutcome::Moved
        }
    }
}

/// Increase or Decrease Indent by `delta`, then, when it changed the
/// document, repair the levels of docxy's own lists in `pkg` that an older
/// build saved without an indent, as the document tab does after one
/// (`repair_list_indents_since`, #1728): stepping such an item's level would
/// otherwise move it in Word by nothing.
fn indent(ed: &mut Editor, pkg: Option<&mut Package>, delta: i32) -> KeyOutcome {
    let out = edit(ed, |ed| ed.change_indent(delta));
    if out.changed() {
        if let Some(pkg) = pkg {
            pkg.repair_list_indents();
        }
    }
    out
}

/// A move: the selection extends with Shift and collapses without, as the
/// document tab's navigation keys do.
fn moved(ed: &mut Editor, shift: bool, f: impl FnOnce(&mut Editor)) -> KeyOutcome {
    ed.extend_selection(shift);
    f(ed);
    KeyOutcome::Moved
}

/// Whether a key's typed text is text (non-empty, not a control character).
fn printable(key_char: Option<&str>) -> Option<&str> {
    key_char.filter(|c| c.chars().next().is_some_and(|ch| !ch.is_control()))
}

/// A key on an editable view, whose package is `pkg`: the document tab's
/// editing map. `macos` picks the Mac's ⌘/⌥ navigation (#1073).
pub(crate) fn edit_key(
    ed: &mut Editor,
    pkg: Option<&mut Package>,
    ks: &Keystroke,
    macos: bool,
) -> KeyOutcome {
    let m = ks.modifiers;
    let key = ks.key.as_str();
    let shift = m.shift;
    let ctrl = m.control || m.platform;
    if m.function {
        return KeyOutcome::Ignored;
    }
    if let Some(nav) = mac_nav::mac_nav(macos, key, m) {
        if nav == mac_nav::MacNav::DeleteWordBack {
            return edit(ed, |ed| {
                mac_nav::run(ed, shift, nav);
            });
        }
        return moved(ed, shift, |ed| {
            mac_nav::run(ed, shift, nav);
        });
    }
    // AltGr (Ctrl+Alt off macOS) and, on macOS, ⌥ make characters: typed.
    // A plain Alt elsewhere is never text.
    let altgr = !macos && m.control && m.alt && !m.platform;
    let typed = printable(ks.key_char.as_deref()).filter(|_| {
        if macos {
            !m.platform && !m.control
        } else {
            altgr || (!ctrl && !m.alt)
        }
    });
    if let Some(text) = typed {
        if key != "enter" && key != "escape" && key != "backspace" && key != "delete" {
            let text = text.to_string();
            return edit(ed, |ed| ed.insert_str(&text));
        }
    }
    if m.alt && !macos {
        return KeyOutcome::Ignored;
    }
    if ctrl {
        return match key {
            "b" | "i" | "u" if !shift => edit(ed, |ed| {
                ed.break_undo_group();
                match key {
                    "b" => ed.toggle_bold(),
                    "i" => ed.toggle_italic(),
                    _ => ed.toggle_underline(),
                }
            }),
            "z" if !shift => history(ed, false).into(),
            "y" | "z" => history(ed, true).into(),
            "a" if !shift => {
                ed.select_all();
                KeyOutcome::Moved
            }
            "c" if !shift => KeyOutcome::Copy,
            "x" if !shift => match ed.cut() {
                Some(clip) => KeyOutcome::Cut(clip),
                None => KeyOutcome::Moved,
            },
            "m" => {
                ed.break_undo_group();
                indent(ed, pkg, if shift { -720 } else { 720 })
            }
            // Ctrl+Tab types a tab in a table cell, where Tab moves (Word).
            "tab" if !shift && ed.in_table() => edit(ed, |ed| {
                ed.break_undo_group();
                ed.insert_tab();
            }),
            "space" if shift => edit(ed, |ed| ed.insert_str("\u{00A0}")),
            "left" => moved(ed, shift, Editor::move_word_left),
            "right" => moved(ed, shift, Editor::move_word_right),
            "home" => moved(ed, shift, Editor::move_doc_start),
            "end" => moved(ed, shift, Editor::move_doc_end),
            _ => KeyOutcome::Ignored,
        };
    }
    match key {
        "escape" if ed.has_selection() => {
            ed.clear_selection();
            KeyOutcome::Moved
        }
        "backspace" => edit(ed, Editor::backspace),
        "delete" => edit(ed, Editor::delete_forward),
        // Shift+Enter: a line break inside the typing step (#853).
        "enter" if shift => edit(ed, Editor::insert_line_break),
        "enter" => edit(ed, |ed| {
            ed.break_undo_group();
            ed.insert_newline();
        }),
        "left" => moved(ed, shift, Editor::move_left),
        "right" => moved(ed, shift, Editor::move_right),
        "home" => moved(ed, shift, Editor::move_home),
        "end" => moved(ed, shift, Editor::move_end),
        "up" => moved(ed, shift, |ed| move_vert(ed, false)),
        "down" => moved(ed, shift, |ed| move_vert(ed, true)),
        _ => KeyOutcome::Ignored,
    }
}

/// Tab (`back`: Shift+Tab) on an editable view, as the document tab's
/// `tab_key` / `shift_tab_key`: in a table cell it moves to the next or
/// previous cell (Tab in the last cell adds a row); elsewhere Tab types a tab
/// and Shift+Tab decreases the indent (a list item a level up).
pub(crate) fn tab(ed: &mut Editor, pkg: Option<&mut Package>, back: bool) -> KeyOutcome {
    if ed.in_table() {
        return edit(ed, |ed| {
            if back {
                ed.table_prev_cell();
            } else {
                ed.table_next_cell();
            }
        });
    }
    ed.break_undo_group();
    if back {
        indent(ed, pkg, -720)
    } else {
        edit(ed, Editor::insert_tab)
    }
}

/// What [`apply_format`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Formatted {
    /// The command was one the view could run (an empty font name, a size
    /// out of range, a malformed colour or an unknown highlight is not).
    pub(crate) ran: bool,
    /// The document changed.
    pub(crate) changed: bool,
}

/// The colour a host gave, as the model keeps it (`RRGGBB`, upper case).
fn hex_color(c: &str) -> Option<String> {
    let c = c.strip_prefix('#').unwrap_or(c);
    (c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit())).then(|| c.to_ascii_uppercase())
}

/// A host's formatting command on the editor (and, for a list, its
/// package): the document tab's ribbon command for it, one undo step.
/// Bullets and Numbering give a package without a numbering part the one
/// that defines their lists ([`Package::add_markdown_numbering`]), so the
/// exported package shows them.
pub(crate) fn apply_format(ed: &mut Editor, pkg: Option<&mut Package>, f: &DocFormat) -> Formatted {
    // A host command is a step of its own: it never grows the typing run
    // before it (`Docxy::with_editor`).
    let ran = |out: KeyOutcome| Formatted {
        ran: true,
        changed: out.changed(),
    };
    let op: Box<dyn FnOnce(&mut Editor)> = match f {
        DocFormat::Bold => Box::new(Editor::toggle_bold),
        DocFormat::Italic => Box::new(Editor::toggle_italic),
        DocFormat::Underline => Box::new(Editor::toggle_underline),
        DocFormat::Font(name) => {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Formatted::default();
            }
            Box::new(move |ed| ed.set_font(&name))
        }
        DocFormat::Size(pt) => {
            let half = (pt * 2.).round();
            if !(2. ..=3276.).contains(&half) {
                return Formatted::default();
            }
            Box::new(move |ed| ed.set_font_size(half as u32))
        }
        DocFormat::Color(c) => {
            let c = match c.as_deref().map(hex_color) {
                None => None,
                Some(Some(c)) => Some(c),
                Some(None) => return Formatted::default(),
            };
            Box::new(move |ed| ed.set_color(c))
        }
        DocFormat::Highlight(h) => {
            if let Some(name) = h.as_deref() {
                if !docxcore::agent::HIGHLIGHT_NAMES.contains(&name) {
                    return Formatted::default();
                }
            }
            let h = h.clone();
            Box::new(move |ed| ed.set_highlight(h))
        }
        DocFormat::Bullets | DocFormat::Numbering => {
            let num = if *f == DocFormat::Bullets {
                NUM_BULLET
            } else {
                NUM_DECIMAL
            };
            if let Some(pkg) = pkg {
                pkg.add_markdown_numbering();
            }
            Box::new(move |ed| ed.set_list((!ed.all_in_list(num)).then_some(num)))
        }
        DocFormat::Indent | DocFormat::Outdent => {
            ed.break_undo_group();
            let delta = if *f == DocFormat::Indent { 720 } else { -720 };
            return ran(indent(ed, pkg, delta));
        }
        DocFormat::Align(a) => {
            let a = *a;
            Box::new(move |ed| ed.set_align(a))
        }
    };
    ed.break_undo_group();
    ran(edit(ed, op))
}

/// The formatting at `ed`'s caret.
pub(crate) fn caret_format(ed: &Editor) -> CaretFormat {
    let r = ed.caret_props_shown();
    CaretFormat {
        bold: r.bold,
        italic: r.italic,
        underline: r.underline,
        strike: r.strike,
        font: r.font,
        size: r.size_half_pts.map(|h| h as f32 / 2.),
        color: r.color,
        highlight: r.highlight,
        bullets: ed.all_in_list(NUM_BULLET),
        numbering: ed.all_in_list(NUM_DECIMAL),
        align: ed.caret_para_props().align,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use docxcore::model::{Block, Run};

    fn para(text: &str) -> Block {
        Block::Paragraph(Paragraph {
            content: vec![Inline::Run(Run {
                text: text.into(),
                ..Run::default()
            })],
            ..Paragraph::default()
        })
    }

    fn editor(paras: &[&str]) -> Editor {
        let mut ed = Editor::new(Document {
            body: paras.iter().map(|p| para(p)).collect(),
        });
        ed.caret = Caret::at(vec![0], 0);
        ed
    }

    /// A key as gpui delivers it: `ctrl-b`, `shift-left`, ... with the typed
    /// text a printable key carries.
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

    fn press(ed: &mut Editor, keys: &str) -> Vec<KeyOutcome> {
        keys.split(' ')
            .map(|k| edit_key(ed, None, &ks(k), false))
            .collect()
    }

    fn type_text(ed: &mut Editor, text: &str) {
        for ch in text.chars() {
            let mut k = Keystroke::parse(if ch == ' ' { "space" } else { "a" }).unwrap();
            k.key = if ch == ' ' {
                "space".into()
            } else {
                ch.to_string()
            };
            k.key_char = Some(ch.to_string());
            assert!(edit_key(ed, None, &k, false).changed(), "{ch:?} typed");
        }
    }

    fn text(ed: &Editor, i: usize) -> String {
        match &ed.doc.body[i] {
            Block::Paragraph(p) => p.plain_text(),
            _ => panic!("a paragraph"),
        }
    }

    fn runs(ed: &Editor, i: usize) -> Vec<(String, bool)> {
        let Block::Paragraph(p) = &ed.doc.body[i] else {
            panic!("a paragraph")
        };
        p.content
            .iter()
            .filter_map(|c| match c {
                Inline::Run(r) => Some((r.text.clone(), r.props.bold)),
                _ => None,
            })
            .collect()
    }

    fn num(ed: &Editor, i: usize) -> (Option<i32>, i32) {
        let Block::Paragraph(p) = &ed.doc.body[i] else {
            panic!("a paragraph")
        };
        (p.props.num_id, p.props.ilvl)
    }

    #[test]
    fn typed_keys_type_and_backspace_delete_and_enter_edit() {
        let mut ed = editor(&[""]);
        type_text(&mut ed, "Helo");
        press(&mut ed, "left");
        type_text(&mut ed, "l");
        assert_eq!(text(&ed, 0), "Hello");
        press(&mut ed, "end backspace");
        assert_eq!(text(&ed, 0), "Hell");
        press(&mut ed, "home delete");
        assert_eq!(text(&ed, 0), "ell");
        press(&mut ed, "end enter");
        type_text(&mut ed, "two");
        assert_eq!(ed.doc.body.len(), 2);
        assert_eq!(text(&ed, 1), "two");
        // Shift+Enter is a line break in the paragraph, not a new one.
        press(&mut ed, "shift-enter");
        assert_eq!(ed.doc.body.len(), 2);
        assert_eq!(text(&ed, 1).chars().count(), 4);
        // Backspace at the document's start changes nothing.
        press(&mut ed, "ctrl-home");
        assert!(matches!(press(&mut ed, "backspace")[0], KeyOutcome::Moved));
    }

    #[test]
    fn a_change_is_told_from_the_blocks_around_the_caret_and_the_length() {
        let mut ed = editor(&["one", "two", "three", "four"]);
        // Backspace at the document's start: nothing to take.
        assert!(matches!(press(&mut ed, "backspace")[0], KeyOutcome::Moved));
        // At a paragraph's start it merges into the one before.
        ed.set_caret(Caret::at(vec![2], 0));
        assert!(press(&mut ed, "backspace")[0].changed());
        assert_eq!(text(&ed, 1), "twothree");
        // Delete at a paragraph's end takes in the next.
        press(&mut ed, "end");
        assert!(press(&mut ed, "delete")[0].changed());
        assert_eq!(text(&ed, 1), "twothreefour");
        // Delete at the document's end: nothing.
        press(&mut ed, "ctrl-end");
        assert!(matches!(press(&mut ed, "delete")[0], KeyOutcome::Moved));
        // A selection from the first paragraph to the last, deleted.
        press(&mut ed, "ctrl-home ctrl-shift-end");
        assert!(press(&mut ed, "delete")[0].changed());
        assert_eq!(ed.doc.body.len(), 1);
        assert_eq!(text(&ed, 0), "");
        // A window at either end of the body stays inside it.
        let ed = editor(&["only"]);
        assert_eq!(edit_window(&ed), 0..1);
    }

    #[test]
    fn undoing_a_step_that_changed_nothing_is_no_change() {
        let mut ed = editor(&["centred"]);
        assert!(apply_format(&mut ed, None, &DocFormat::Align(Align::Center)).changed);
        // Center again on the centred paragraph still makes a step.
        let serial = ed.undo_serial();
        let f = apply_format(&mut ed, None, &DocFormat::Align(Align::Center));
        assert!(f.ran && !f.changed);
        assert_ne!(ed.undo_serial(), serial, "a step to undo");
        let undone = history(&mut ed, false);
        assert!(undone.ran && !undone.changed);
        let redone = history(&mut ed, true);
        assert!(redone.ran && !redone.changed);
        // Ctrl+Z on it, likewise: taken, no change; the next takes back the
        // first Center, which is one.
        assert!(matches!(press(&mut ed, "ctrl-z")[0], KeyOutcome::Moved));
        assert!(press(&mut ed, "ctrl-z")[0].changed());
        // With nothing left to undo nothing runs.
        assert_eq!(history(&mut ed, false), Formatted::default());
    }

    #[test]
    fn shift_arrows_select_and_typing_replaces_the_selection() {
        let mut ed = editor(&["Alpha Beta"]);
        press(&mut ed, "shift-right shift-right");
        assert_eq!(
            ed.copy().map(|c| c.to_text()).as_deref(),
            Some("Al"),
            "two characters selected"
        );
        type_text(&mut ed, "X");
        assert_eq!(text(&ed, 0), "Xpha Beta");
        // Ctrl+Shift+Right selects a word; Escape collapses, and with nothing
        // selected it is left to the suite.
        press(&mut ed, "ctrl-shift-right");
        assert!(ed.has_selection());
        assert!(matches!(press(&mut ed, "escape")[0], KeyOutcome::Moved));
        assert!(!ed.has_selection());
        assert!(matches!(press(&mut ed, "escape")[0], KeyOutcome::Ignored));
        press(&mut ed, "ctrl-a");
        assert_eq!(ed.copy().map(|c| c.to_text()).as_deref(), Some("Xpha Beta"));
    }

    #[test]
    fn ctrl_z_undoes_only_the_typing_after_a_caret_move() {
        let mut ed = editor(&["one", "two"]);
        press(&mut ed, "end");
        type_text(&mut ed, "AB");
        // A move ends the typing run: the next typing is a step of its own.
        press(&mut ed, "down end");
        type_text(&mut ed, "CD");
        // A click places the caret as `set_caret` does: that ends a run too.
        ed.set_caret(Caret::at(vec![0], 0));
        type_text(&mut ed, "EF");
        assert_eq!(
            (text(&ed, 0), text(&ed, 1)),
            ("EFoneAB".into(), "twoCD".into())
        );
        assert!(press(&mut ed, "ctrl-z")[0].changed());
        assert_eq!(
            (text(&ed, 0), text(&ed, 1)),
            ("oneAB".into(), "twoCD".into())
        );
        press(&mut ed, "ctrl-z");
        assert_eq!((text(&ed, 0), text(&ed, 1)), ("oneAB".into(), "two".into()));
        // Redo (Ctrl+Y, and Ctrl+Shift+Z) brings the steps back one by one.
        assert!(press(&mut ed, "ctrl-y")[0].changed());
        assert_eq!(text(&ed, 1), "twoCD");
        assert!(press(&mut ed, "ctrl-shift-z")[0].changed());
        assert_eq!(text(&ed, 0), "EFoneAB");
        // With nothing to redo the key is taken and changes nothing.
        assert!(matches!(press(&mut ed, "ctrl-y")[0], KeyOutcome::Moved));
    }

    #[test]
    fn a_bold_command_then_typing_gives_a_bold_run_and_one_undo_takes_back_the_last_step() {
        let mut ed = editor(&["Alpha "]);
        press(&mut ed, "end");
        // At a collapsed caret Bold is for what is typed next: no change yet.
        let f = apply_format(&mut ed, None, &DocFormat::Bold);
        assert!(f.ran && !f.changed);
        assert!(caret_format(&ed).bold);
        type_text(&mut ed, "Beta");
        assert_eq!(
            runs(&ed, 0),
            [("Alpha ".into(), false), ("Beta".into(), true)]
        );
        // The bold run's caret reads bold, the plain one's does not.
        ed.set_caret(Caret::at(vec![0], 8));
        assert!(caret_format(&ed).bold);
        ed.set_caret(Caret::at(vec![0], 2));
        assert!(!caret_format(&ed).bold);
        // Bullets, then one undo: the list goes, the bold typing stays.
        assert!(apply_format(&mut ed, None, &DocFormat::Bullets).changed);
        assert_eq!(num(&ed, 0), (Some(NUM_BULLET), 0));
        assert!(caret_format(&ed).bullets);
        assert!(press(&mut ed, "ctrl-z")[0].changed());
        assert_eq!(num(&ed, 0).0, None);
        assert_eq!(
            runs(&ed, 0),
            [("Alpha ".into(), false), ("Beta".into(), true)]
        );
    }

    #[test]
    fn ctrl_b_i_u_toggle_over_a_selection() {
        let mut ed = editor(&["Alpha"]);
        press(&mut ed, "ctrl-a");
        assert!(press(&mut ed, "ctrl-b")[0].changed());
        assert!(runs(&ed, 0)[0].1);
        press(&mut ed, "ctrl-i ctrl-u");
        let f = caret_format(&ed);
        assert!(f.bold && f.italic && f.underline);
        press(&mut ed, "ctrl-b");
        assert!(!caret_format(&ed).bold);
        // Each toggle was its own step.
        press(&mut ed, "ctrl-z");
        assert!(caret_format(&ed).bold);
    }

    #[test]
    fn each_format_command_changes_the_selection() {
        let mut ed = editor(&["Alpha", "Beta"]);
        press(&mut ed, "ctrl-a");
        assert!(apply_format(&mut ed, None, &DocFormat::Font("Georgia".into())).changed);
        assert!(apply_format(&mut ed, None, &DocFormat::Size(14.4)).changed);
        assert!(apply_format(&mut ed, None, &DocFormat::Color(Some("#1f7a3d".into()))).changed);
        assert!(apply_format(&mut ed, None, &DocFormat::Highlight(Some("yellow".into()))).changed);
        assert!(apply_format(&mut ed, None, &DocFormat::Align(Align::Center)).changed);
        let f = caret_format(&ed);
        assert_eq!(f.font.as_deref(), Some("Georgia"));
        assert_eq!(f.size, Some(14.5));
        assert_eq!(f.color.as_deref(), Some("1F7A3D"));
        assert_eq!(f.highlight.as_deref(), Some("yellow"));
        assert_eq!(f.align, Align::Center);
        assert!(apply_format(&mut ed, None, &DocFormat::Color(None)).changed);
        assert!(apply_format(&mut ed, None, &DocFormat::Highlight(None)).changed);
        let f = caret_format(&ed);
        assert_eq!((f.color, f.highlight), (None, None));
        // What the view cannot run is refused and changes nothing.
        for bad in [
            DocFormat::Font(" ".into()),
            DocFormat::Size(0.),
            DocFormat::Size(2000.),
            DocFormat::Color(Some("green".into())),
            DocFormat::Highlight(Some("mauve".into())),
        ] {
            assert_eq!(
                apply_format(&mut ed, None, &bad),
                Formatted::default(),
                "{bad:?}"
            );
        }
        // Numbering over both paragraphs, then off again: every selected
        // paragraph was in it.
        assert!(apply_format(&mut ed, None, &DocFormat::Numbering).changed);
        assert_eq!(
            (num(&ed, 0).0, num(&ed, 1).0),
            (Some(NUM_DECIMAL), Some(NUM_DECIMAL))
        );
        assert!(caret_format(&ed).numbering);
        assert!(apply_format(&mut ed, None, &DocFormat::Numbering).changed);
        assert_eq!((num(&ed, 0).0, num(&ed, 1).0), (None, None));
    }

    #[test]
    fn bullets_toggle_off_only_when_every_selected_paragraph_has_them() {
        let mut ed = editor(&["a", "b"]);
        apply_format(&mut ed, None, &DocFormat::Bullets);
        assert_eq!((num(&ed, 0).0, num(&ed, 1).0), (Some(NUM_BULLET), None));
        press(&mut ed, "ctrl-a");
        apply_format(&mut ed, None, &DocFormat::Bullets);
        assert_eq!(
            (num(&ed, 0).0, num(&ed, 1).0),
            (Some(NUM_BULLET), Some(NUM_BULLET))
        );
        apply_format(&mut ed, None, &DocFormat::Bullets);
        assert_eq!((num(&ed, 0).0, num(&ed, 1).0), (None, None));
    }

    #[test]
    fn indent_steps_a_list_item_and_says_so_and_tab_follows_the_document_tab() {
        let mut ed = editor(&["item", "plain"]);
        apply_format(&mut ed, None, &DocFormat::Bullets);
        let f = apply_format(&mut ed, None, &DocFormat::Indent);
        assert!(f.ran && f.changed);
        assert_eq!(num(&ed, 0), (Some(NUM_BULLET), 1));
        assert!(press(&mut ed, "ctrl-m")[0].changed());
        assert_eq!(num(&ed, 0).1, 2);
        press(&mut ed, "ctrl-shift-m");
        assert_eq!(num(&ed, 0).1, 1);
        // Shift+Tab decreases the indent; Tab types a tab, as in a document tab.
        assert!(tab(&mut ed, None, true).changed());
        assert_eq!(num(&ed, 0).1, 0);
        assert!(tab(&mut ed, None, false).changed());
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!()
        };
        assert!(p.content.iter().any(|c| matches!(c, Inline::Tab(_))));
        // Outdent with nothing to take back changes nothing.
        ed.set_caret(Caret::at(vec![1], 0));
        let f = apply_format(&mut ed, None, &DocFormat::Outdent);
        assert!(f.ran && !f.changed);
    }

    /// A package whose docxy bullet list (`numId` 9990, as `ensure_list`
    /// makes it) an older build saved without level indents.
    fn package_with_old_bullets() -> Package {
        let id = docxcore::package::BULLET_LIST_NUM_ID;
        let levels: String = (0..9)
            .map(|l| {
                format!(
                    "<w:lvl w:ilvl=\"{l}\"><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"•\"/></w:lvl>"
                )
            })
            .collect();
        let xml = format!(
            "<?xml version=\"1.0\"?><w:numbering xmlns:w=\"x\">\
             <w:abstractNum w:abstractNumId=\"{id}\">{levels}</w:abstractNum>\
             <w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"{id}\"/></w:num></w:numbering>"
        );
        let mut pkg = docxcore::package::new_package(Document { body: vec![] });
        pkg.add_markdown_numbering();
        assert!(pkg.set_part("word/numbering.xml", xml.into_bytes()));
        pkg
    }

    #[test]
    fn an_indent_repairs_an_old_lists_levels_by_command_key_and_shift_tab_1728() {
        let id = docxcore::package::BULLET_LIST_NUM_ID;
        let has_ind = |pkg: &Package| {
            pkg.part_text("word/numbering.xml")
                .is_some_and(|x| x.contains("<w:ind w:left=\"720\""))
        };
        let list_editor = || {
            let mut ed = editor(&["item"]);
            if let Block::Paragraph(p) = &mut ed.doc.body[0] {
                p.props.num_id = Some(id);
            }
            ed
        };
        // Increase Indent from the host, Ctrl+M, and Shift+Tab after a step
        // in: each leaves the levels with their indents.
        let mut pkg = package_with_old_bullets();
        assert!(!has_ind(&pkg));
        let mut ed = list_editor();
        assert!(apply_format(&mut ed, Some(&mut pkg), &DocFormat::Indent).changed);
        assert!(has_ind(&pkg), "{:?}", pkg.part_text("word/numbering.xml"));
        let mut pkg = package_with_old_bullets();
        let mut ed = list_editor();
        assert!(edit_key(&mut ed, Some(&mut pkg), &ks("ctrl-m"), false).changed());
        assert!(has_ind(&pkg));
        let mut pkg = package_with_old_bullets();
        let mut ed = list_editor();
        if let Block::Paragraph(p) = &mut ed.doc.body[0] {
            p.props.ilvl = 1;
        }
        assert!(tab(&mut ed, Some(&mut pkg), true).changed());
        assert!(has_ind(&pkg));
        // An indent that changes nothing leaves the part as it was.
        let mut pkg = package_with_old_bullets();
        let mut ed = editor(&["plain"]);
        assert!(!apply_format(&mut ed, Some(&mut pkg), &DocFormat::Outdent).changed);
        assert!(!has_ind(&pkg));
    }

    #[test]
    fn a_list_command_gives_a_package_its_numbering_once() {
        let mut ed = editor(&["a"]);
        let mut pkg = docxcore::package::new_package(Document { body: vec![] });
        assert!(pkg.part("word/numbering.xml").is_none());
        apply_format(&mut ed, Some(&mut pkg), &DocFormat::Bullets);
        let numbering = pkg
            .part_text("word/numbering.xml")
            .expect("a numbering part");
        assert!(numbering.contains("w:numId=\"1\""), "{numbering}");
        assert!(numbering.contains("w:numId=\"2\""), "{numbering}");
        // A package that has one keeps it as it is.
        let before = pkg.part("word/numbering.xml").map(<[u8]>::to_vec);
        apply_format(&mut ed, Some(&mut pkg), &DocFormat::Numbering);
        assert_eq!(pkg.part("word/numbering.xml").map(<[u8]>::to_vec), before);
        // Bold needs no package part.
        let mut other = docxcore::package::new_package(Document { body: vec![] });
        apply_format(&mut ed, Some(&mut other), &DocFormat::Bold);
        assert!(other.part("word/numbering.xml").is_none());
    }

    #[test]
    fn the_suites_keys_and_alt_letters_are_left_alone_and_altgr_types() {
        let mut ed = editor(&["x"]);
        for k in [
            "ctrl-s", "ctrl-v", "ctrl-w", "f5", "ctrl-p", "alt-f", "ctrl-f1",
        ] {
            assert!(
                matches!(edit_key(&mut ed, None, &ks(k), false), KeyOutcome::Ignored),
                "{k}"
            );
        }
        assert_eq!(text(&ed, 0), "x");
        // Copy and cut: Ctrl+X takes the selection away and hands it back.
        press(&mut ed, "ctrl-a");
        assert!(matches!(press(&mut ed, "ctrl-c")[0], KeyOutcome::Copy));
        let out = press(&mut ed, "ctrl-x").remove(0);
        let KeyOutcome::Cut(clip) = out else {
            panic!("{out:?}")
        };
        assert_eq!(clip.to_text(), "x");
        assert_eq!(text(&ed, 0), "");
        // AltGr (Ctrl+Alt with typed text) types it.
        let mut k = Keystroke::parse("ctrl-alt-q").unwrap();
        k.key_char = Some("@".into());
        assert!(edit_key(&mut ed, None, &k, false).changed());
        assert_eq!(text(&ed, 0), "@");
        // On macOS ⌥ makes a character, ⌘ never does, and ⌘← goes to the
        // line's start.
        let mut k = Keystroke::parse("alt-2").unwrap();
        k.key_char = Some("™".into());
        assert!(edit_key(&mut ed, None, &k, true).changed());
        let mut k = Keystroke::parse("cmd-q").unwrap();
        k.key_char = Some("q".into());
        assert!(matches!(
            edit_key(&mut ed, None, &k, true),
            KeyOutcome::Ignored
        ));
        assert_eq!(text(&ed, 0), "@™");
        assert!(matches!(
            edit_key(&mut ed, None, &Keystroke::parse("cmd-left").unwrap(), true),
            KeyOutcome::Moved
        ));
        assert_eq!(ed.caret.offset, 0);
    }
}
