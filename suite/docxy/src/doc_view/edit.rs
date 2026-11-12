//! The editing mode of the embedded document view (#1947): what a key, a
//! Tab, a host's formatting command, its insert and cut (#1977), a picture
//! and a paste (#1953) do to the view's [`Editor`] and package, and
//! what the caret's formatting reads. It is the document tab's own map
//! (`Docxy::on_key`, `apply_doc_act`, `tab_key` / `shift_tab_key`,
//! `do_paste`) without the suite's extras (AutoCorrect, Repeat, fields,
//! breaks, Paste Special), over a plain editor and package, so it is tested
//! without a window.

use super::*;
use docxcore::editor::{Clip, FIELD_CHAR, LINE_BREAK};
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
    /// Ctrl+X cut this, which the view writes to the clipboard, and whether
    /// the document changed (a cut across containers only collapses, see
    /// [`cut`]).
    Cut(Clip, bool),
    /// Ctrl+V (and Ctrl+Shift+V, as a document tab takes it): the view
    /// reads the clipboard and pastes ([`paste_source`], #1953).
    Paste,
}

impl KeyOutcome {
    /// Whether the document changed.
    pub(crate) fn changed(&self) -> bool {
        matches!(self, KeyOutcome::Changed | KeyOutcome::Cut(_, true))
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
    /// Make the selection a hyperlink to `url` showing `text` (#1952), as a
    /// host's Insert Link dialog does; at a collapsed caret, insert one.
    /// `url` is trimmed; `#name` links to the bookmark `name`. When `text`
    /// is the selection's own text ([`DocView::selected_text`], which the
    /// host's dialog prefills) and the selection is text of one paragraph,
    /// the selected text keeps its formatting; otherwise the selection is
    /// replaced by `text` (one line: line ends and tabs become spaces). An
    /// empty `text` shows the URL. The link is underlined and coloured
    /// `0563C1` (unless the text has a colour). Inside a link the new one
    /// splits it: links never nest. The caret ends after it. Not run for an
    /// empty URL, a bare `#` or one holding a control character.
    InsertLink {
        text: String,
        url: String,
    },
    /// Edit the link at the caret ([`CaretFormat::link`]): link it to `url`
    /// and, when `text` is not empty and differs from the link's text, show
    /// `text` in the formatting of the link's first character. The caret
    /// ends after the link. Not run off a link or for a URL
    /// [`DocFormat::InsertLink`] refuses; run with no change (no undo step,
    /// no [`DocView::on_change`]) for the link's own URL and text.
    EditLink {
        text: String,
        url: String,
    },
    /// Remove the link at the caret ([`CaretFormat::link`]): its text stays,
    /// without the link's underline and colour. Not run off a link.
    RemoveLink,
}

/// A hyperlink as a host's link dialog shows it ([`CaretFormat::link`]).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct DocLink {
    /// The link's text.
    pub text: String,
    /// Its target URL, `#` and its bookmark for a link inside the
    /// document, or `target#anchor` for a link with both (a URL's fragment
    /// as Word stores it).
    pub url: String,
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
    /// The link at the caret (#1952), for a host's Edit Link and Remove Link
    /// and to prefill its dialog: the one typing at the caret goes into, so
    /// a caret just after a link is on it, and one at its start after other
    /// text is not (a link that opens the paragraph holds a caret at 0);
    /// over a selection, the link holding the whole selection. `None` off a
    /// link.
    pub link: Option<DocLink>,
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

/// What a host's text keeps on its way into the document
/// ([`insert_text`]): line ends made `\n` (`\r\n` and a lone `\r`, which
/// [`Clip::from_text`] would drop), and only the characters a `w:t` can
/// hold. The writer escapes `& < >` alone, so a C0 control other than tab,
/// newline and [`LINE_BREAK`] (Word's plain-text manual line break), or
/// U+FFFE / U+FFFF, would make the saved part ill-formed XML. A field's
/// stand-in ([`FIELD_CHAR`]) is not text either.
fn insertable(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|&ch| match ch {
            '\t' | '\n' | LINE_BREAK => true,
            '\u{0}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}' | FIELD_CHAR => false,
            _ => true,
        })
        .collect()
}

/// A host's Insert (a signature, a quick part) at the caret, replacing the
/// selection: [`Editor::paste_text_only`], Paste Text Only's own path, so
/// newlines split paragraphs, tabs become tab inlines, every run takes the
/// formatting typing at the caret would (a format toggled there for what is
/// typed next too) and the caret ends after it. A [`LINE_BREAK`] is a line
/// break, as Shift+Enter makes it, inside the same step. One undo step of
/// its own: [`Editor::one_step`] ends the typing run before and after it,
/// so neither the typing before nor the typing after joins it. Nothing runs
/// for a text with nothing [`insertable`] (the selection stays).
pub(crate) fn insert_text(ed: &mut Editor, text: &str) -> Formatted {
    let text = insertable(text);
    if text.is_empty() {
        return Formatted::default();
    }
    let out = edit(ed, |ed| {
        if !text.contains(LINE_BREAK) {
            ed.paste_text_only(&text);
            return;
        }
        // Inside the outer step the pieces' own steps are its: the first
        // piece (or the break, when the text starts with one) replaces the
        // selection, and each lands after the one before.
        ed.one_step("Paste", |ed| {
            for (i, piece) in text.split(LINE_BREAK).enumerate() {
                if i > 0 {
                    ed.insert_line_break();
                }
                if !piece.is_empty() {
                    ed.paste_text_only(piece);
                }
            }
        });
    });
    Formatted {
        ran: true,
        changed: out.changed(),
    }
}

/// Cut, by Ctrl+X or the host's [`DocView::cut`]: the selection's clip and
/// whether the document changed, as the document tab's Cut, one undo step
/// "Cut" ([`Editor::one_step`]); `None`, and nothing done, with nothing to
/// cut. A selection across containers (body text into a table cell) is
/// copied but not deleted: [`Editor::cut`] only collapses it, with no step,
/// so the document has not changed.
pub(crate) fn cut(ed: &mut Editor) -> Option<(Clip, bool)> {
    let mut clip = None;
    let out = edit(ed, |ed| ed.one_step("Cut", |ed| clip = ed.cut()));
    clip.map(|clip| (clip, out.changed()))
}

/// The package a view given none is given for its first picture (#1953):
/// [`docxcore::package::new_package`]'s, whose styles part is empty, so
/// nothing the document shows changes. Its `document` is empty: the host
/// exports [`DocView::document`] with it, as with any package.
fn fresh_package() -> Package {
    docxcore::package::new_package(Document { body: vec![] })
}

/// The picture a host's bytes are (#1953): what [`docxcore::image::sniff`]
/// reads (PNG, JPEG, GIF or BMP), when `content_type` names the same kind.
/// The type is read as a header gives it: any case, parameters after `;`
/// dropped, with the kinds' common aliases. `None` for bytes no picture,
/// a type of another kind, or one that is not a picture's.
pub(crate) fn picture_kind(bytes: &[u8], content_type: &str) -> Option<docxcore::image::ImageInfo> {
    let info = docxcore::image::sniff(bytes)?;
    let ty = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let ext = match ty.as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" | "image/pjpeg" => "jpeg",
        "image/gif" => "gif",
        "image/bmp" | "image/x-bmp" | "image/x-ms-bmp" => "bmp",
        _ => return None,
    };
    (ext == info.ext).then_some(info)
}

/// The text width (EMUs) a picture inserted at the caret is scaled down to
/// when wider, as Insert Picture does: the caret's section's, the last
/// section's through [`print_sheet::last_sect`] (the body's trailing
/// sectPr, else the package's, else Letter with 1" margins), so a document
/// with no sectPr, an HTML import's, still caps it.
fn caret_text_width_emu(ed: &Editor, pkg: Option<&Package>) -> i64 {
    let sections = ed.sections();
    let i = ed.caret_section();
    let raw = match sections.get(i) {
        Some(raw) if i + 1 < sections.len() => raw.as_str(),
        _ => print_sheet::last_sect(&ed.doc, pkg),
    };
    let g = docxcore::model::PageGeom::from_sect_pr(raw);
    i64::from((g.w - g.ml - g.mr).max(1)) * 635
}

/// Every `r:embed` id in `xml`, each once.
fn embed_ids(xml: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in xml.split("r:embed=\"").skip(1) {
        if let Some(id) = piece.split('"').next().filter(|id| !id.is_empty()) {
            if !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// A host's picture (`info`, [`picture_kind`] of `bytes`) at the caret,
/// replacing the selection, as Insert Picture inserts one (#1953): a new
/// `word/media/imageN.<ext>` part related from the body, the picture its
/// pixels at 96 dpi scaled to the text width ([`caret_text_width_emu`]),
/// one undo step "Insert Picture". A view with no package is given one
/// ([`fresh_package`]). The media part is added outside the step, as a
/// document tab's Insert Picture and paste add theirs: an undo leaves it
/// unreferenced. Its relationship id is none a picture of the body
/// already names, so a picture whose id the package never defined does
/// not start to draw this one. Not run when the package cannot take the
/// part (one without a body part).
pub(crate) fn insert_picture(
    ed: &mut Editor,
    pkg: &mut Option<Package>,
    bytes: &[u8],
    info: &docxcore::image::ImageInfo,
) -> Formatted {
    let max_cx = caret_text_width_emu(ed, pkg.as_ref());
    let (cx, cy) = docxcore::image::extent_emu(info, Some(max_cx));
    let body = docxcore::serialize::blocks_to_xml(&ed.doc.body);
    let pkg = pkg.get_or_insert_with(fresh_package);
    // Unique among the body's drawings and every story part's.
    let mut xmls = vec![body];
    for name in pkg.part_names() {
        if name.starts_with("word/") && name.ends_with(".xml") && name != BODY_PART {
            xmls.extend(pkg.part_text(name));
        }
    }
    let id = docxcore::image::next_drawing_id(xmls.iter().map(String::as_str));
    let taken = embed_ids(&xmls[0]);
    let Some(rid) = pkg.add_part_image_avoiding(BODY_PART, bytes, info, &taken) else {
        return Formatted::default();
    };
    let run = docxcore::image::inline_picture_run(&rid, cx, cy, id, &format!("Picture {id}"));
    let out = edit(ed, |ed| {
        ed.one_step("Insert Picture", |e| {
            e.insert_object(vec![Inline::Raw(run)])
        })
    });
    Formatted {
        ran: true,
        changed: out.changed(),
    }
}

/// What a paste in the view inserts (#1953).
#[derive(Debug, PartialEq)]
pub(crate) enum Pasted {
    /// A clip, as a document tab's paste takes it ([`doc_paste_from`]).
    Clip(PasteFrom, Clip),
    /// Another app's picture: its bytes, a kind [`docxcore::image::sniff`]
    /// reads.
    Picture(Vec<u8>),
}

/// What Ctrl+V pastes in the view (#1953): a document tab's choice
/// ([`doc_paste_from`]: the view's own clip while it is still ours, else
/// the RTF, else the HTML, else the text), with the clipboard's `image`
/// before the HTML when no text is beside it. A browser's Copy Image puts
/// an `<img>` HTML beside its picture, and Word pastes the picture. Ours
/// never writes an image, so an image proves another copy: then only the
/// RTF goes before it. An image beside text pastes the text, but gpui's X11
/// client hands back only the image when both are offered (it asks for
/// image kinds first), while Wayland reads the text first and Windows gives
/// both: the same copy may paste the picture on X11 and the text elsewhere.
/// An image `sniff` cannot read (WebP, SVG, TIFF, ...) counts as none, so
/// the HTML or text after it still pastes.
pub(crate) fn paste_source(
    ours: Option<&DocClip>,
    now: &ClipRead,
    rtf: Option<&[u8]>,
    image: Option<Vec<u8>>,
    html: impl FnOnce() -> Option<String>,
) -> Option<Pasted> {
    let image = image.filter(|b| docxcore::image::sniff(b).is_some());
    if let Some(image) = image.filter(|_| now.text().is_none()) {
        let rtf = rtf.and_then(|rtf| Clip::from_rtf(rtf, None));
        return Some(match rtf {
            Some(clip) => Pasted::Clip(PasteFrom::Rtf, clip),
            None => Pasted::Picture(image),
        });
    }
    doc_paste_from(ours, now, rtf, html).map(|(from, clip)| Pasted::Clip(from, clip))
}

/// Paste `clip` at the caret, replacing the selection, as a document tab's
/// Paste: one undo step "Paste". The view's own clip (`media`: its
/// pictures' media, read where it was copied) brings its pictures into the
/// view's package ([`pictures::carry`], which reuses a part with the same
/// bytes), giving a view with none a package first when a picture needs
/// it.
pub(crate) fn paste_clip(
    ed: &mut Editor,
    pkg: &mut Option<Package>,
    mut clip: Clip,
    media: Option<&pictures::Media>,
) -> Formatted {
    if let Some(media) = media.filter(|m| !m.is_empty()) {
        if clip.inline_embeds().iter().any(|r| media.contains_key(r)) {
            let pkg = pkg.get_or_insert_with(fresh_package);
            pictures::carry(pkg, BODY_PART, media, &mut clip);
        }
    }
    let out = edit(ed, |ed| ed.one_step("Paste", |e| e.paste(&clip)));
    Formatted {
        ran: true,
        changed: out.changed(),
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

/// The text a key types, if it types any: AltGr (Ctrl+Alt off macOS) and,
/// on macOS, ⌥ make characters; a plain Alt elsewhere is never text, nor is
/// a Ctrl (⌘) chord. Enter, Escape, Backspace and Delete are never text,
/// whatever their `key_char`. The editing map and the Find box (#1887) both
/// type by it.
pub(crate) fn typed_text(ks: &Keystroke, macos: bool) -> Option<&str> {
    let m = ks.modifiers;
    let ctrl = m.control || m.platform;
    let altgr = !macos && m.control && m.alt && !m.platform;
    printable(ks.key_char.as_deref())
        .filter(|_| {
            if macos {
                !m.platform && !m.control
            } else {
                altgr || (!ctrl && !m.alt)
            }
        })
        .filter(|_| !matches!(ks.key.as_str(), "enter" | "escape" | "backspace" | "delete"))
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
    if let Some(text) = typed_text(ks, macos) {
        let text = text.to_string();
        return edit(ed, |ed| ed.insert_str(&text));
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
            // Ctrl+Shift+V too, as the document tab's map takes it.
            "v" => KeyOutcome::Paste,
            "x" if !shift => match cut(ed) {
                Some((clip, changed)) => KeyOutcome::Cut(clip, changed),
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
        DocFormat::InsertLink { text, url } => {
            return link_step(ed, |ed| ed.insert_hyperlink(text, url));
        }
        DocFormat::EditLink { text, url } => {
            return link_step(ed, |ed| ed.edit_hyperlink(text, url));
        }
        DocFormat::RemoveLink => return link_step(ed, Editor::remove_hyperlink),
    };
    ed.break_undo_group();
    ran(edit(ed, op))
}

/// A link command, which may not run (a refused URL, no link at the caret):
/// a step of its own, as every host command is, run when `f` says so.
fn link_step(ed: &mut Editor, f: impl FnOnce(&mut Editor) -> bool) -> Formatted {
    ed.break_undo_group();
    let mut ran = false;
    let out = edit(ed, |ed| ran = f(ed));
    Formatted {
        ran,
        changed: ran && out.changed(),
    }
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
        link: ed.caret_link().map(|l| DocLink {
            text: l.text,
            url: l.url,
        }),
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
            "ctrl-s",
            "ctrl-w",
            "f5",
            "ctrl-p",
            "alt-f",
            "ctrl-f1",
            "ctrl-alt-v",
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
        let KeyOutcome::Cut(clip, true) = out else {
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

    fn texts(ed: &Editor) -> Vec<String> {
        (0..ed.doc.body.len()).map(|i| text(ed, i)).collect()
    }

    /// A selection from `from` to `to` in the first paragraph.
    fn select(ed: &mut Editor, from: usize, to: usize) {
        ed.anchor = Some(Caret::at(vec![0], from));
        ed.caret = Caret::at(vec![0], to);
    }

    #[test]
    fn insert_text_at_a_caret_splits_paragraphs_and_one_undo_takes_it_back_1977() {
        for given in ["a\nb", "a\r\nb", "a\rb"] {
            let mut ed = editor(&["xy"]);
            ed.caret = Caret::at(vec![0], 1);
            let done = insert_text(&mut ed, given);
            assert_eq!(
                done,
                Formatted {
                    ran: true,
                    changed: true
                },
                "{given:?}"
            );
            assert_eq!(texts(&ed), ["xa", "by"], "{given:?}");
            assert_eq!(ed.caret, Caret::at(vec![1], 1), "{given:?}");
            assert_eq!(
                history(&mut ed, false),
                Formatted {
                    ran: true,
                    changed: true
                }
            );
            assert_eq!(texts(&ed), ["xy"], "{given:?}");
        }
    }

    #[test]
    fn insert_text_replaces_the_selection_1977() {
        let mut ed = editor(&["Alpha Beta"]);
        select(&mut ed, 6, 10);
        assert!(insert_text(&mut ed, "Gamma").changed);
        assert_eq!(texts(&ed), ["Alpha Gamma"]);
        assert!(!ed.has_selection());
        assert_eq!(ed.caret, Caret::at(vec![0], 11));
        history(&mut ed, false);
        assert_eq!(texts(&ed), ["Alpha Beta"]);
    }

    #[test]
    fn insert_text_takes_the_typing_format_1977() {
        // Inside a bold run it is bold.
        let mut ed = editor(&["ab"]);
        press(&mut ed, "ctrl-a ctrl-b");
        ed.clear_selection();
        ed.set_caret(Caret::at(vec![0], 1));
        insert_text(&mut ed, "Q\nR");
        assert_eq!(texts(&ed), ["aQ", "Rb"]);
        for i in 0..2 {
            assert!(
                runs(&ed, i).iter().all(|(_, bold)| *bold),
                "{:?}",
                runs(&ed, i)
            );
        }
        // A Bold toggled at a collapsed caret, for what is typed next.
        let mut ed = editor(&["ab"]);
        ed.set_caret(Caret::at(vec![0], 1));
        press(&mut ed, "ctrl-b");
        insert_text(&mut ed, "Q");
        assert_eq!(
            runs(&ed, 0),
            [("a".into(), false), ("Q".into(), true), ("b".into(), false)]
        );
    }

    #[test]
    fn insert_text_is_its_own_step_between_typing_1977() {
        let mut ed = editor(&[""]);
        type_text(&mut ed, "zz");
        insert_text(&mut ed, "Q");
        type_text(&mut ed, "w");
        assert_eq!(texts(&ed), ["zzQw"]);
        for want in ["zzQ", "zz", ""] {
            assert!(history(&mut ed, false).changed);
            assert_eq!(texts(&ed), [want]);
        }
    }

    #[test]
    fn insert_text_of_nothing_runs_nothing_1977() {
        let mut ed = editor(&["Alpha Beta"]);
        select(&mut ed, 6, 10);
        for nothing in ["", "\u{1}", "\u{FFFF}\u{0C}"] {
            assert_eq!(insert_text(&mut ed, nothing), Formatted::default());
            assert_eq!(texts(&ed), ["Alpha Beta"]);
            assert!(ed.has_selection());
        }
        assert!(ed.undo_serial().is_none(), "no step");
    }

    #[test]
    fn insert_text_drops_what_xml_cannot_hold_1977() {
        let mut ed = editor(&[""]);
        insert_text(&mut ed, "a\u{1}b\u{0C}c\u{FFFF}\u{FFFE}\u{1F}d\u{FFFC}e");
        assert_eq!(texts(&ed), ["abcde"]);
        // A tab stays a tab inline.
        insert_text(&mut ed, "\tf");
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!("a paragraph")
        };
        assert!(
            p.content.iter().any(|i| matches!(i, Inline::Tab(_))),
            "{:?}",
            p.content
        );
        assert_eq!(p.plain_text(), "abcde\tf");
    }

    #[test]
    fn insert_text_makes_a_vertical_tab_a_line_break_1977() {
        use docxcore::model::BreakKind;
        let mut ed = editor(&["xy"]);
        ed.caret = Caret::at(vec![0], 1);
        press(&mut ed, "ctrl-b");
        let steps = ed.undo_serial();
        assert!(insert_text(&mut ed, "a\u{0B}b").changed);
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!("a paragraph")
        };
        let shape: Vec<_> = p
            .content
            .iter()
            .map(|i| match i {
                Inline::Run(r) => format!("{}{}", r.text, if r.props.bold { "*" } else { "" }),
                Inline::Break(BreakKind::Line, props) => {
                    format!("|{}", if props.bold { "*" } else { "" })
                }
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(shape, ["x", "a*", "|*", "b*", "y"]);
        assert_eq!(ed.caret.offset, 4, "after the b");
        // One step, and over a selection a leading break replaces it.
        history(&mut ed, false);
        assert_eq!(texts(&ed), ["xy"]);
        assert_eq!(ed.undo_serial(), steps, "one step");
        select(&mut ed, 0, 2);
        insert_text(&mut ed, "\u{0B}c");
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!("a paragraph")
        };
        assert!(
            matches!(p.content[0], Inline::Break(BreakKind::Line, _)),
            "{:?}",
            p.content
        );
        assert_eq!(p.plain_text().chars().last(), Some('c'));
        assert_eq!(p.content.len(), 2, "{:?}", p.content);
        history(&mut ed, false);
        assert_eq!(texts(&ed), ["xy"]);
    }

    #[test]
    fn host_cut_takes_the_selection_as_one_step_1977() {
        let mut ed = editor(&["Alpha Beta"]);
        type_text(&mut ed, "z");
        select(&mut ed, 0, 6);
        let (clip, changed) = cut(&mut ed).expect("a selection");
        assert!(changed);
        assert_eq!(clip.to_text(), "zAlpha");
        assert_eq!(texts(&ed), [" Beta"]);
        // Ctrl+X takes the same clip.
        let mut keyed = editor(&["Alpha Beta"]);
        type_text(&mut keyed, "z");
        select(&mut keyed, 0, 6);
        let KeyOutcome::Cut(key_clip, true) = press(&mut keyed, "ctrl-x").remove(0) else {
            panic!("a cut")
        };
        assert_eq!(key_clip.to_text(), clip.to_text());
        assert_eq!(texts(&keyed), texts(&ed));
        // The step is the document tab's "Cut".
        assert_eq!(ed.undo_names().first().map(String::as_str), Some("Cut"));
        // One undo restores the cut, not the typing before it.
        history(&mut ed, false);
        assert_eq!(texts(&ed), ["zAlpha Beta"]);
        // Nothing selected: nothing cut, no step.
        ed.clear_selection();
        let steps = ed.undo_serial();
        assert!(cut(&mut ed).is_none());
        assert_eq!(texts(&ed), ["zAlpha Beta"]);
        assert_eq!(ed.undo_serial(), steps);
    }

    #[test]
    fn a_cut_across_containers_copies_but_changes_nothing_1977() {
        use docxcore::model::{Cell, Row, Table, VMerge};
        let table = Block::Table(Table {
            grid: vec![100],
            rows: vec![Row {
                cells: vec![Cell {
                    grid_span: 1,
                    v_merge: VMerge::None,
                    blocks: vec![para("Cell")],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        let fresh = || {
            let mut ed = Editor::new(Document {
                body: vec![para("Body"), table.clone()],
            });
            ed.anchor = Some(Caret::at(vec![0], 1));
            ed.caret = Caret::at(vec![1, 0, 0, 0], 2);
            ed
        };
        let mut ed = fresh();
        let before = ed.doc.body.clone();
        let (clip, changed) = cut(&mut ed).expect("something to copy");
        assert!(!clip.to_text().is_empty());
        assert!(!changed);
        assert_eq!(ed.doc.body, before);
        assert!(ed.undo_serial().is_none(), "no step");
        // Ctrl+X: a cut, not a change.
        let mut keyed = fresh();
        let out = press(&mut keyed, "ctrl-x").remove(0);
        assert!(matches!(out, KeyOutcome::Cut(_, false)), "{out:?}");
        assert!(!out.changed());
        assert_eq!(keyed.doc.body, before);
    }

    /// #1952: the host's link commands report whether they ran and whether
    /// the document changed, each one undo step, and the caret's link reads
    /// back.
    #[test]
    fn link_commands_run_change_and_read_back_1952() {
        let insert = |text: &str, url: &str| DocFormat::InsertLink {
            text: text.into(),
            url: url.into(),
        };
        let mut ed = editor(&["Say Beta now"]);
        select(&mut ed, 4, 8);
        assert_eq!(caret_format(&ed).link, None);
        // A URL it refuses: not run, the selection kept.
        let out = apply_format(&mut ed, None, &insert("Beta", " "));
        assert_eq!((out.ran, out.changed), (false, false));
        assert!(ed.has_selection());
        let out = apply_format(&mut ed, None, &insert("Beta", "https://b.example/"));
        assert_eq!((out.ran, out.changed), (true, true));
        let link = caret_format(&ed)
            .link
            .expect("the caret is at the link's end");
        assert_eq!(
            (link.text.as_str(), link.url.as_str()),
            ("Beta", "https://b.example/")
        );
        assert!(caret_format(&ed).underline);
        // The same text and URL: run, nothing changed, no step.
        let steps = ed.undo_names().len();
        let edit_to = |text: &str, url: &str| DocFormat::EditLink {
            text: text.into(),
            url: url.into(),
        };
        let out = apply_format(&mut ed, None, &edit_to("Beta", "https://b.example/"));
        assert_eq!((out.ran, out.changed), (true, false));
        assert_eq!(ed.undo_names().len(), steps);
        let out = apply_format(&mut ed, None, &edit_to("Gamma", "#top"));
        assert_eq!((out.ran, out.changed), (true, true));
        assert_eq!(text(&ed, 0), "Say Gamma now");
        assert_eq!(
            caret_format(&ed).link.map(|l| l.url).as_deref(),
            Some("#top")
        );
        let out = apply_format(&mut ed, None, &DocFormat::RemoveLink);
        assert_eq!((out.ran, out.changed), (true, true));
        assert_eq!(caret_format(&ed).link, None);
        assert_eq!(text(&ed, 0), "Say Gamma now");
        // Off a link Edit and Remove do not run.
        let out = apply_format(&mut ed, None, &DocFormat::RemoveLink);
        assert_eq!((out.ran, out.changed), (false, false));
        let out = apply_format(&mut ed, None, &edit_to("x", "https://x.example/"));
        assert_eq!((out.ran, out.changed), (false, false));
        // Three steps, taken back one at a time.
        assert_eq!(
            ed.undo_names(),
            ["Remove Hyperlink", "Edit Hyperlink", "Hyperlink"]
        );
        assert!(history(&mut ed, false).changed);
        assert_eq!(
            caret_format(&ed).link.map(|l| l.text).as_deref(),
            Some("Gamma")
        );
        assert!(history(&mut ed, false).changed);
        assert_eq!(text(&ed, 0), "Say Beta now");
        assert!(history(&mut ed, false).changed);
        assert!(ed.doc.body[0] == para("Say Beta now"));
    }

    /// #1952: typed text right after a new link goes into it, so the caret
    /// there is on the link; the next undo step takes the typing alone.
    #[test]
    fn typing_after_a_new_link_extends_it_1952() {
        let mut ed = editor(&["Hi "]);
        ed.caret = Caret::at(vec![0], 3);
        let out = apply_format(
            &mut ed,
            None,
            &DocFormat::InsertLink {
                text: String::new(),
                url: "https://u.example/".into(),
            },
        );
        assert!(out.ran && out.changed);
        type_text(&mut ed, "x");
        assert_eq!(
            caret_format(&ed).link.map(|l| l.text).as_deref(),
            Some("https://u.example/x")
        );
        assert!(history(&mut ed, false).changed);
        assert_eq!(text(&ed, 0), "Hi https://u.example/");
    }

    // ---- pictures and paste (#1953) ---------------------------------------

    /// A PNG header `sniff` reads as `w` x `h` (the pixels are never decoded).
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b
    }

    const PNG_INFO: &str = "image/png";

    fn picture_rids(ed: &Editor, i: usize) -> Vec<String> {
        let Block::Paragraph(p) = &ed.doc.body[i] else {
            panic!("a paragraph")
        };
        p.content
            .iter()
            .filter_map(|c| match c {
                Inline::Raw(raw) => docxcore::image::inline_embed(raw),
                _ => None,
            })
            .collect()
    }

    fn put(ed: &mut Editor, pkg: &mut Option<Package>, bytes: &[u8]) -> Formatted {
        let info = picture_kind(bytes, PNG_INFO).expect("a PNG");
        insert_picture(ed, pkg, bytes, &info)
    }

    #[test]
    fn a_host_picture_goes_into_the_package_and_is_one_undo_step_1953() {
        let mut ed = editor(&["Alpha Beta"]);
        let mut pkg = Some(docxcore::package::new_package(Document { body: vec![] }));
        type_text(&mut ed, "z");
        // Over a selection: the picture replaces it.
        select(&mut ed, 1, 6);
        let bytes = png(3, 2);
        let done = put(&mut ed, &mut pkg, &bytes);
        assert_eq!(
            done,
            Formatted {
                ran: true,
                changed: true
            }
        );
        assert_eq!(text(&ed, 0).replace('\u{FFFC}', ""), "z Beta");
        let rids = picture_rids(&ed, 0);
        assert_eq!(rids.len(), 1);
        let pkg_ref = pkg.as_ref().unwrap();
        let target = pkg_ref.part_rel_target(BODY_PART, &rids[0]).unwrap();
        assert_eq!(target, "word/media/image1.png");
        assert_eq!(pkg_ref.part(&target), Some(&bytes[..]));
        assert_eq!(
            pkg_ref.part_content_type(&target).as_deref(),
            Some("image/png")
        );
        // Its pixels at 96 dpi: 3 x 2 px.
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!()
        };
        let raw = p
            .content
            .iter()
            .find_map(|c| match c {
                Inline::Raw(raw) => Some(raw.clone()),
                _ => None,
            })
            .unwrap();
        assert!(raw.contains(r#"cx="28575" cy="19050""#), "{raw}");
        // One step of its own, named as Insert Picture's.
        assert_eq!(
            ed.undo_names().first().map(String::as_str),
            Some("Insert Picture")
        );
        assert!(history(&mut ed, false).changed);
        assert_eq!(texts(&ed), ["zAlpha Beta"]);
        assert!(picture_rids(&ed, 0).is_empty());
        // The typing before it is a step of its own.
        assert!(history(&mut ed, false).changed);
        assert_eq!(texts(&ed), ["Alpha Beta"]);
        // A second picture: its own part and id, and a drawing id past the
        // first's.
        assert!(history(&mut ed, true).changed);
        assert!(history(&mut ed, true).changed);
        put(&mut ed, &mut pkg, &png(5, 5));
        let rids2 = picture_rids(&ed, 0);
        assert_eq!(rids2.len(), 2);
        assert_ne!(rids2[0], rids2[1]);
        let xml = docxcore::serialize::blocks_to_xml(&ed.doc.body);
        assert_eq!(xml.matches("docPr id=\"1\"").count(), 1, "{xml}");
        assert_eq!(xml.matches("docPr id=\"2\"").count(), 1, "{xml}");
    }

    #[test]
    fn only_a_picture_of_the_kind_its_type_names_is_inserted_1953() {
        let bytes = png(3, 2);
        for ty in ["image/png", "IMAGE/PNG", " image/png; charset=binary "] {
            assert!(picture_kind(&bytes, ty).is_some(), "{ty}");
        }
        let gif = b"GIF89a\x02\0\x02\0\0\0\0".to_vec();
        assert_eq!(picture_kind(&gif, "image/gif").map(|i| i.ext), Some("gif"));
        assert!(picture_kind(&gif, "image/png").is_none(), "disagrees");
        assert!(picture_kind(&bytes, "image/jpeg").is_none(), "disagrees");
        assert!(picture_kind(&bytes, "image/webp").is_none(), "not ours");
        assert!(picture_kind(&bytes, "").is_none());
        let webp = b"RIFF\x10\0\0\0WEBPVP8 ".to_vec();
        assert!(picture_kind(&webp, "image/webp").is_none());
        assert!(picture_kind(b"not a picture", "image/png").is_none());
    }

    #[test]
    fn a_view_with_no_package_is_given_one_and_its_look_stays_1953() {
        use docxcore::model::{Cell, Row, Table, VMerge};
        let bold = Block::Paragraph(Paragraph {
            content: vec![Inline::Run(Run {
                text: "Bold".into(),
                props: docxcore::model::RunProps {
                    bold: true,
                    ..Default::default()
                },
            })],
            ..Paragraph::default()
        });
        let table = Block::Table(Table {
            grid: vec![100],
            rows: vec![Row {
                cells: vec![Cell {
                    grid_span: 1,
                    v_merge: VMerge::None,
                    blocks: vec![bold],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut ed = Editor::new(Document {
            body: vec![para("Top"), table],
        });
        ed.set_styles_xml(None);
        ed.caret = Caret::at(vec![1, 0, 0, 0], 2);
        let before = caret_format(&ed);
        assert!(before.bold);
        let mut pkg: Option<Package> = None;
        ed.caret = Caret::at(vec![0], 3);
        assert!(put(&mut ed, &mut pkg, &png(3, 2)).changed);
        let pkg = pkg.expect("given a package");
        assert!(
            pkg.part_rel_target(BODY_PART, &picture_rids(&ed, 0)[0])
                .is_some()
        );
        // The view seeds the new package's styles before the next edit.
        let styles = pkg
            .part("word/styles.xml")
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(std::sync::Arc::from);
        assert!(styles.is_some());
        ed.set_styles_xml(styles);
        ed.caret = Caret::at(vec![1, 0, 0, 0], 2);
        assert_eq!(caret_format(&ed), before);
    }

    #[test]
    fn a_wide_picture_is_capped_at_the_text_width_without_a_sect_pr_1953() {
        let mut ed = editor(&["x"]);
        let mut pkg = None;
        put(&mut ed, &mut pkg, &png(2000, 1000));
        let xml = docxcore::serialize::blocks_to_xml(&ed.doc.body);
        // Letter with 1" margins: 9360 twips of text, 635 EMUs each.
        let width = 9360 * 635;
        assert!(
            xml.contains(&format!(r#"cx="{width}" cy="{}""#, width / 2)),
            "{xml}"
        );
        // The section the caret is in decides: a first section half as wide.
        let mut first = para("narrow");
        if let Block::Paragraph(p) = &mut first {
            p.props.section_break = Some(
                r#"<w:sectPr><w:pgSz w:w="6120" w:h="15840"/><w:pgMar w:left="1440" w:right="1440"/></w:sectPr>"#
                    .into(),
            );
        }
        let mut ed = Editor::new(Document {
            body: vec![first, para("wide")],
        });
        ed.caret = Caret::at(vec![0], 0);
        put(&mut ed, &mut pkg, &png(2000, 1000));
        let xml = docxcore::serialize::blocks_to_xml(&ed.doc.body[..1]);
        assert!(xml.contains(&format!(r#"cx="{}""#, 3240 * 635)), "{xml}");
    }

    #[test]
    fn a_new_picture_takes_no_id_an_orphan_picture_names_1953() {
        // A document handed over without its package: its picture names
        // rId2, which the fresh package does not define.
        let mut ed = editor(&["x"]);
        let orphan = docxcore::image::inline_picture_run("rId2", 9525, 9525, 7, "Old");
        ed.caret = Caret::at(vec![0], 1);
        ed.insert_object(vec![Inline::Raw(orphan)]);
        let mut pkg = None;
        put(&mut ed, &mut pkg, &png(1, 1));
        let rids = picture_rids(&ed, 0);
        assert_eq!(rids.len(), 2);
        assert_eq!(rids[0], "rId2", "the orphan keeps its id");
        assert_ne!(rids[1], "rId2");
        let pkg = pkg.unwrap();
        assert!(pkg.part_rel_target(BODY_PART, "rId2").is_none());
        assert!(pkg.part_rel_target(BODY_PART, &rids[1]).is_some());
        // Its drawing id is past the orphan's too.
        let xml = docxcore::serialize::blocks_to_xml(&ed.doc.body);
        assert!(xml.contains("docPr id=\"8\""), "{xml}");
    }

    // The paste source (#1953).

    fn ours(text: &str) -> DocClip {
        DocClip {
            clip: Clip::from_text("ours"),
            text: text.into(),
            media: Default::default(),
        }
    }

    const RTF_BOLD: &[u8] = br"{\rtf1\ansi {\b Bold} plain}";
    const HTML: &str = "<p>From <b>the web</b></p>";

    fn from(src: Option<Pasted>) -> Option<&'static str> {
        src.map(|s| match s {
            Pasted::Clip(PasteFrom::Ours, _) => "ours",
            Pasted::Clip(PasteFrom::Rtf, _) => "rtf",
            Pasted::Clip(PasteFrom::Html, _) => "html",
            Pasted::Clip(PasteFrom::Text, _) => "text",
            Pasted::Picture(_) => "picture",
        })
    }

    #[test]
    fn paste_takes_ours_then_rtf_then_an_image_alone_then_html_then_text_1953() {
        let text = |t: &str| ClipRead::Text(t.into());
        let img = || Some(png(3, 2));
        let html = || Some(HTML.to_string());
        let none = || None;
        // Ours while the clipboard holds its text.
        let o = ours("Beta");
        assert_eq!(
            from(paste_source(Some(&o), &text("Beta"), None, None, none)),
            Some("ours")
        );
        // Another copy's RTF beside its text.
        assert_eq!(
            from(paste_source(
                Some(&o),
                &text("Bold plain"),
                Some(RTF_BOLD),
                None,
                none
            )),
            Some("rtf")
        );
        // HTML with no text beside it (a browser's).
        assert_eq!(
            from(paste_source(None, &ClipRead::NotText, None, None, html)),
            Some("html")
        );
        // Plain text.
        assert_eq!(
            from(paste_source(None, &text("plain"), None, None, none)),
            Some("text")
        );
        // An image alone pastes as a picture, its bytes as read.
        let Some(Pasted::Picture(bytes)) =
            paste_source(None, &ClipRead::NotText, None, img(), none)
        else {
            panic!("a picture")
        };
        assert_eq!(bytes, png(3, 2));
        // A browser's Copy Image: the picture beside its <img> HTML.
        assert_eq!(
            from(paste_source(None, &ClipRead::NotText, None, img(), html)),
            Some("picture")
        );
        // The RTF goes before the image.
        assert_eq!(
            from(paste_source(
                None,
                &ClipRead::NotText,
                Some(RTF_BOLD),
                img(),
                html
            )),
            Some("rtf")
        );
        // Text beside the image (Windows, Wayland): the text.
        assert_eq!(
            from(paste_source(None, &text("cell"), None, img(), none)),
            Some("text")
        );
        // After our picture-only copy (its text U+FFFC), another app's image
        // is not ours.
        let pic = ours("\u{FFFC}");
        assert_eq!(
            from(paste_source(
                Some(&pic),
                &ClipRead::NotText,
                None,
                img(),
                none
            )),
            Some("picture")
        );
        // Even a clip recorded with no text: an image proves another copy.
        let empty = ours("");
        assert_eq!(
            from(paste_source(
                Some(&empty),
                &ClipRead::NotText,
                None,
                img(),
                none
            )),
            Some("picture")
        );
        // Nothing at all.
        assert_eq!(
            from(paste_source(None, &ClipRead::Nothing, None, None, none)),
            None
        );
    }

    #[test]
    fn an_image_sniff_cannot_read_counts_as_none_1953() {
        let webp = || Some(b"RIFF\x10\0\0\0WEBPVP8 ".to_vec());
        let html = || Some(HTML.to_string());
        assert_eq!(
            from(paste_source(None, &ClipRead::NotText, None, webp(), html)),
            Some("html")
        );
        assert_eq!(
            from(paste_source(None, &ClipRead::NotText, None, webp(), || {
                None
            })),
            None
        );
    }

    #[test]
    fn ctrl_v_and_ctrl_shift_v_paste_and_ctrl_alt_v_is_left_1953() {
        let mut ed = editor(&["x"]);
        assert!(matches!(press(&mut ed, "ctrl-v")[0], KeyOutcome::Paste));
        assert!(matches!(
            press(&mut ed, "ctrl-shift-v")[0],
            KeyOutcome::Paste
        ));
        assert!(matches!(
            press(&mut ed, "ctrl-alt-v")[0],
            KeyOutcome::Ignored
        ));
        // ⌘V on macOS.
        let k = Keystroke::parse("cmd-v").unwrap();
        assert!(matches!(
            edit_key(&mut ed, None, &k, true),
            KeyOutcome::Paste
        ));
        // The key changes nothing itself: the view pastes.
        assert_eq!(texts(&ed), ["x"]);
        assert!(!KeyOutcome::Paste.changed());
    }

    #[test]
    fn a_pasted_clip_is_one_step_and_formatting_stays_1953() {
        let mut ed = editor(&["Alpha"]);
        let mut pkg = None;
        type_text(&mut ed, "z");
        let clip = Clip::from_rtf(RTF_BOLD, None).expect("an RTF clip");
        let done = paste_clip(&mut ed, &mut pkg, clip, None);
        assert!(done.ran && done.changed);
        assert!(
            runs(&ed, 0).contains(&("Bold".into(), true)),
            "{:?}",
            runs(&ed, 0)
        );
        assert_eq!(ed.undo_names().first().map(String::as_str), Some("Paste"));
        assert!(pkg.is_none(), "text needs no package");
        history(&mut ed, false);
        assert_eq!(texts(&ed), ["zAlpha"]);
    }

    #[test]
    fn our_clip_with_a_picture_brings_its_media_once_1953() {
        // Copy a picture from a view with a package.
        let mut src = editor(&["ab"]);
        let mut src_pkg = None;
        src.caret = Caret::at(vec![0], 1);
        put(&mut src, &mut src_pkg, &png(3, 2));
        select(&mut src, 0, 3);
        let clip = src.copy().expect("a selection");
        let media = pictures::capture(src_pkg.as_ref().unwrap(), BODY_PART, &clip);
        assert_eq!(media.len(), 1);
        // Paste it into a view with no package: it is given one, with the
        // picture's part.
        let mut dst = editor(&["Z"]);
        let mut dst_pkg = None;
        dst.caret = Caret::at(vec![0], 1);
        let done = paste_clip(&mut dst, &mut dst_pkg, clip.clone(), Some(&media));
        assert!(done.changed);
        let rids = picture_rids(&dst, 0);
        assert_eq!(rids.len(), 1);
        let pkg = dst_pkg.as_ref().expect("given a package");
        let target = pkg.part_rel_target(BODY_PART, &rids[0]).unwrap();
        assert_eq!(pkg.part(&target), Some(&png(3, 2)[..]));
        // Pasted again: the same part, no second copy of the bytes.
        let parts = pkg.part_names().len();
        paste_clip(&mut dst, &mut dst_pkg, clip, Some(&media));
        let pkg = dst_pkg.as_ref().unwrap();
        assert_eq!(pkg.part_names().len(), parts);
        let rids = picture_rids(&dst, 0);
        assert_eq!(rids.len(), 2);
        assert_eq!(rids[0], rids[1]);
    }
}
