//! The Header & Footer tab's Insert group and Insert Alignment Tab (#768,
//! PAG-065, PAG-073): Date & Time..., Document Info, Quick Parts' Document
//! Property and Field..., Pictures..., Online Pictures... and the Alignment
//! Tab dialog. Each inserts at the caret of the header or footer being
//! edited (Pictures... needs one open; the rest insert in the body when none
//! is), as one undo step on that editor.
//!
//! A picture goes in as a media part related from the edited part's own
//! rels (`word/_rels/header1.xml.rels`) and an inline `w:drawing` run.
//! Undoing it takes the run out and leaves the media part and its
//! relationship in the package, which is harmless (as a removed design's
//! part is). The suite draws a picture as a frame of its size; the picture
//! itself shows in Word, the terminal app and the browser editor.
use super::*;
use crate::dialog::catalog;
use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::page_setup::{choice, chosen, ok_cancel};
use docxcore::field::{DateTime, FieldContext};
use docxcore::sdt::CoreProp;
use hf_tab::HfAct;
use std::path::Path;

/// The Date and Time dialog's formats, Word's en-US list in its order
/// (Word 2024; the spec marks the order unverified). Word may write `TIME`
/// rather than `DATE` for the time-only pictures when Update automatically
/// is on [unverified]; both show the same value.
pub(crate) const DATE_FORMATS: [&str; 17] = [
    "M/d/yyyy",
    "dddd, MMMM d, yyyy",
    "MMMM d, yyyy",
    "M/d/yy",
    "yyyy-MM-dd",
    "d-MMM-yy",
    "M.d.yyyy",
    "MMM. d, yy",
    "d MMMM yyyy",
    "MMMM yy",
    "MMM-yy",
    "M/d/yyyy h:mm am/pm",
    "M/d/yyyy h:mm:ss am/pm",
    "h:mm am/pm",
    "h:mm:ss am/pm",
    "HH:mm",
    "HH:mm:ss",
];

/// The `DATE` field instruction for a picture.
fn date_instr(picture: &str) -> String {
    format!("DATE \\@ \"{picture}\"")
}

/// `now` in each of [`DATE_FORMATS`], through the field evaluator, so the
/// static text and a field's shown value are the same.
pub(crate) fn date_choices(now: &DateTime) -> Vec<String> {
    let ctx = FieldContext {
        now: Some(now.clone()),
        ..Default::default()
    };
    DATE_FORMATS
        .iter()
        .map(|p| docxcore::field::eval_field_ctx(&date_instr(p), &ctx).unwrap_or_default())
        .collect()
}

/// The Date and Time dialog at `now`: Available formats and Update
/// automatically (clear, as Word opens it).
pub(crate) fn date_time_dialog(now: &DateTime) -> Dialog {
    let mut d = Dialog::message(
        catalog::HF_DATE_TIME,
        "Date and Time",
        String::new(),
        &[],
        DialogOwner::HfDateTime,
    );
    d.text = None;
    let choices = date_choices(now);
    let items: Vec<&str> = choices.iter().map(String::as_str).collect();
    d.controls = vec![
        choice(
            "formats",
            "&Available formats:",
            ControlKind::Dropdown,
            &items,
            Some(0),
            None,
        ),
        Control::new(
            "update",
            "&Update automatically",
            ControlKind::Checkbox,
            Value::Bool(false),
        ),
    ];
    d.buttons = ok_cancel();
    d.mark_opened();
    d
}

/// OK on Date and Time: the chosen format's text at the caret, or with
/// Update automatically a `DATE` field showing it.
pub(crate) fn apply_date_time(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    let i = chosen(d, "formats").ok_or("Choose a format")?;
    let picture = DATE_FORMATS.get(i).ok_or("Choose a format")?;
    let text = d
        .controls
        .iter()
        .find(|c| c.name == "formats")
        .and_then(|c| c.items.get(i))
        .cloned()
        .unwrap_or_default();
    let props = ed.caret_props();
    let inline = if d.value("update") == Some(&Value::Bool(true)) {
        docxcore::field::fld_simple(&date_instr(picture), &text, &props)
    } else {
        Inline::Run(docxcore::model::Run { text, props })
    };
    ed.break_undo_group();
    ed.paste(&Clip {
        paras: vec![vec![inline]],
    });
    Ok(true)
}

/// The Alignment Tab dialog's choices: what each shows, and its OOXML value.
pub(crate) const ALIGNMENTS: [(&str, &str); 3] =
    [("Left", "left"), ("Center", "center"), ("Right", "right")];
pub(crate) const RELATIVE_TO: [(&str, &str); 2] = [("Margin", "margin"), ("Indent", "indent")];
pub(crate) const LEADERS: [(&str, &str); 5] = [
    ("(none)", "none"),
    ("........", "dot"),
    ("--------", "hyphen"),
    ("________", "underscore"),
    (
        "\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}",
        "middleDot",
    ),
];

/// The Alignment Tab dialog (Word's, PAG-073): Alignment, Align relative
/// to and Leader, opening on Left, Margin and none.
pub(crate) fn alignment_tab_dialog() -> Dialog {
    let mut d = Dialog::message(
        catalog::HF_ALIGNMENT_TAB,
        "Alignment Tab",
        String::new(),
        &[],
        DialogOwner::HfAlignmentTab,
    );
    d.text = None;
    let labels = |t: &[(&'static str, &str)]| t.iter().map(|x| x.0).collect::<Vec<_>>();
    d.controls = vec![
        choice(
            "alignment",
            "Alignment",
            ControlKind::Radio,
            &labels(&ALIGNMENTS),
            Some(0),
            None,
        ),
        choice(
            "relative",
            "Align &relative to:",
            ControlKind::Dropdown,
            &labels(&RELATIVE_TO),
            Some(0),
            None,
        ),
        choice(
            "leader",
            "Leader",
            ControlKind::Radio,
            &labels(&LEADERS),
            Some(0),
            None,
        ),
    ];
    d.buttons = ok_cancel();
    d.mark_opened();
    d
}

/// The alignment tab run the dialog's choices make, in `props`.
pub(crate) fn ptab_xml(d: &Dialog, props: &docxcore::model::RunProps) -> String {
    let of = |name: &str, t: &[(&str, &'static str)]| {
        chosen(d, name).and_then(|i| t.get(i)).unwrap_or(&t[0]).1
    };
    docxcore::serialize::ptab_run_xml(
        props,
        of("alignment", &ALIGNMENTS),
        of("relative", &RELATIVE_TO),
        of("leader", &LEADERS),
    )
}

/// OK on Alignment Tab: the tab at the caret, in the caret's formatting.
pub(crate) fn apply_alignment_tab(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    let run = ptab_xml(d, &ed.caret_props());
    ed.break_undo_group();
    ed.paste(&Clip {
        paras: vec![vec![Inline::Raw(run)]],
    });
    Ok(true)
}

/// The Online Pictures button's label in its dialog: it runs Pictures....
pub(crate) const FROM_A_FILE: &str = "From a File...";

/// Online Pictures...: docxy does not search the web, so its dialog offers
/// a picture from a file instead.
pub(crate) fn online_pictures_dialog() -> Dialog {
    let mut d = Dialog::message(
        catalog::HF_ONLINE_PICTURES,
        "Online Pictures",
        "docxy does not search the web for pictures. Insert a picture from a file instead.".into(),
        &[],
        DialogOwner::HfOnlinePictures,
    );
    d.buttons = vec![
        Button {
            default: true,
            ..Button::new(FROM_A_FILE, ButtonRole::Accept)
        },
        Button::new("Cancel", ButtonRole::Cancel),
    ];
    d.mark_opened();
    d
}

/// The XML of every story `tab` holds, as it is now: the body and the open
/// header/footer editors, and every part of the package's `word/` folder
/// (the other headers and footers, notes, comments). Ids unique across a
/// document are drawn from it.
fn story_xmls(tab: &DocTab) -> Vec<String> {
    let mut out = Vec::new();
    if let Surface::Doc(ed) = &tab.surface {
        out.push(docxcore::serialize::blocks_to_xml(&ed.doc.body));
    }
    if let Some(h) = &tab.hf_edit {
        out.push(docxcore::serialize::blocks_to_xml(&h.editor.doc.body));
    }
    if let Some(pkg) = &tab.pkg {
        for name in pkg.part_names() {
            if name.starts_with("word/") && name.ends_with(".xml") {
                out.extend(pkg.part_text(name));
            }
        }
    }
    out
}

/// The document property control for `prop` at the caret of `tab`'s edited
/// story: bound to the property, showing its value from
/// `docProps/core.xml` (or the placeholder), with an id no other control
/// in the document has.
pub(crate) fn property_control(tab: &DocTab, prop: CoreProp) -> Vec<Inline> {
    let core = tab
        .pkg
        .as_ref()
        .and_then(|p| p.part_text("docProps/core.xml"))
        .unwrap_or_default();
    let props = docxcore::field::parse_core_props(&core);
    let value = prop.value(&props, &core);
    let xmls = story_xmls(tab);
    let id = docxcore::sdt::next_control_id(xmls.iter().map(String::as_str));
    let run_props = tab_edit_target_ref(tab)
        .map(Editor::caret_props)
        .unwrap_or_default();
    docxcore::sdt::core_property_control(prop, &value, id, &run_props)
}

/// Insert the picture at `path` at the caret of the open header or footer.
/// The status on success; an error (nothing inserted) when no header or
/// footer is open or the file is not a picture docxy can size.
pub(crate) fn insert_picture(tab: &mut DocTab, path: &Path) -> Result<String, String> {
    let Some(h) = tab.hf_edit.as_ref() else {
        return Err("Pictures... inserts into the header or footer being edited".into());
    };
    let (part, section) = (h.part_name.clone(), h.section);
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read {name}: {e}"))?;
    let info = docxcore::image::sniff(&bytes)
        .ok_or_else(|| format!("{name} is not a PNG, JPEG, GIF or BMP picture"))?;
    // Scaled down to the section's text width when wider, as Word does.
    let max_cx = match &tab.surface {
        Surface::Doc(ed) => ed.sections().get(section).map(|raw| {
            let g = docxcore::model::PageGeom::from_sect_pr(raw);
            i64::from((g.w - g.ml - g.mr).max(1)) * 635
        }),
        _ => None,
    };
    let (cx, cy) = docxcore::image::extent_emu(&info, max_cx);
    let xmls = story_xmls(tab);
    let id = docxcore::image::next_drawing_id(xmls.iter().map(String::as_str));
    let pkg = tab.pkg.as_mut().ok_or("Pictures need a .docx")?;
    let rid = pkg
        .add_part_image(&part, &bytes, &info)
        .ok_or("Could not add the picture to the header or footer part")?;
    let run = docxcore::image::inline_picture_run(&rid, cx, cy, id, &name);
    let ed = &mut tab.hf_edit.as_mut().expect("checked above").editor;
    ed.break_undo_group();
    ed.paste(&Clip {
        paras: vec![vec![Inline::Raw(run)]],
    });
    tab.mark_dirty();
    Ok(format!("Inserted picture {name}"))
}

/// The status a harness instance shows for Pictures....
pub(crate) const PICTURES_HARNESS: &str =
    "Pictures... opens a native file dialog: use the harness hf-insert-picture verb";

impl Docxy {
    /// Dispatch an Insert group command or Insert Alignment Tab.
    pub(crate) fn hf_insert_act(
        &mut self,
        act: HfAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every one of them edits the document, its dialogs on OK.
        if self.protected_refused(cx) {
            return self.refocus(window, cx);
        }
        match act {
            HfAct::DateTime => {
                let now = crate::local_clock();
                self.push_hf_dialog(date_time_dialog(&now));
            }
            HfAct::AlignmentTab => self.push_hf_dialog(alignment_tab_dialog()),
            HfAct::OnlinePictures => self.push_hf_dialog(online_pictures_dialog()),
            HfAct::FileName(path) => {
                return if path {
                    self.insert_field("FILENAME \\p", "", window, cx)
                } else {
                    self.insert_field("FILENAME", "", window, cx)
                };
            }
            HfAct::Property(prop) => {
                let Some(tab) = self.tabs.get(self.active) else {
                    return;
                };
                let control = property_control(tab, prop);
                return self.with_editor(window, cx, |e| {
                    e.paste(&Clip {
                        paras: vec![control],
                    })
                });
            }
            HfAct::Pictures => return self.pick_hf_picture(window, cx),
            _ => {}
        }
        self.refocus(window, cx);
    }

    /// Open one of the Insert group's dialogs over the active document.
    fn push_hf_dialog(&mut self, d: Dialog) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if matches!(tab.surface, Surface::Doc(_)) {
                tab.dialogs.push(d);
            } else {
                tab.status = "This command inserts into a document".into();
            }
        }
    }

    /// Pictures...: the native Insert Picture file dialog, then the picture
    /// into the open header or footer. A harness instance never opens a
    /// native dialog: `hf-insert-picture` hands the path in.
    pub(crate) fn pick_hf_picture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .tabs
            .get(self.active)
            .is_none_or(|t| t.hf_edit.is_none())
        {
            self.set_status("Pictures... inserts into the header or footer being edited");
            return self.refocus(window, cx);
        }
        if self.harness {
            self.set_status(PICTURES_HARNESS);
            return self.refocus(window, cx);
        }
        let picked = crate::macos_menu::native_modal(|| {
            rfd::FileDialog::new()
                .set_title("Insert Picture")
                .add_filter(
                    "All Pictures (*.png, *.jpg, *.jpeg, *.gif, *.bmp)",
                    &["png", "jpg", "jpeg", "gif", "bmp"],
                )
                .pick_file()
        });
        if let Some(path) = picked {
            self.insert_hf_picture(&path, window, cx);
        }
        self.refocus(window, cx);
    }

    /// The picture at `path` into the open header or footer, refused in
    /// Protected View and the like as any edit is. The status says what
    /// happened.
    pub(crate) fn insert_hf_picture(
        &mut self,
        path: &Path,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.protected_refused(cx) {
            return false;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        let done = insert_picture(tab, path);
        let ok = done.is_ok();
        tab.status = done.unwrap_or_else(|e| e).into();
        cx.notify();
        ok
    }

    /// A press on the Online Pictures dialog: From a File... closes it and
    /// runs Pictures...; Cancel closes it. `None` for any other dialog.
    pub(crate) fn online_pictures_click(
        &mut self,
        button: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let tab = self.tabs.get_mut(self.active)?;
        match online_pictures_press(tab, button)? {
            Ok(true) => self.pick_hf_picture(window, cx),
            Ok(false) => {}
            Err(e) => return Some(Err(e)),
        }
        Some(Ok(()))
    }
}

/// [`Docxy::online_pictures_click`] on the tab: the dialog closes on either
/// button; `Ok(true)` when it was From a File..., which runs Pictures....
/// `None` when Online Pictures is not the top dialog.
pub(crate) fn online_pictures_press(
    tab: &mut DocTab,
    button: &str,
) -> Option<Result<bool, String>> {
    if tab.dialogs.top()?.owner != DialogOwner::HfOnlinePictures {
        return None;
    }
    if button != FROM_A_FILE && button != "Cancel" {
        return Some(Err(format!("the dialog has no button '{button}'")));
    }
    tab.dialogs.pop();
    Some(Ok(button == FROM_A_FILE))
}

#[cfg(test)]
mod tests;
