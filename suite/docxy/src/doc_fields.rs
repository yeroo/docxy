//! Word's field keys in a document (#1241): F9 updates the fields in the
//! selection (or the one at the caret), Alt+F9 shows every field's code
//! instead of its result, and Shift+F9 opens Edit Field on the field at the
//! caret.
//!
//! Word's Shift+F9 toggles one field's code in place. Here a field is one
//! unit (#642) whose code cannot be typed into, so Shift+F9 opens a dialog
//! holding the code instead; Alt+F9 already shows every code.

use crate::dialog::catalog;
use crate::dialog::{ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::page_setup::{ok_cancel, text_of};
use crate::{DocTab, Docxy, tab_edit_target, tab_field_context};
use gpui::{Context, Window};

/// What a field key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKey {
    /// F9: Update Field.
    Update,
    /// Alt+F9: show field codes, or results again.
    ToggleCodes,
    /// Shift+F9: Edit Field.
    Edit,
}

/// The field key `key` with these modifiers is, on a document. Ctrl+F9
/// (insert a field) and Alt+Shift+F9 are not field keys here.
pub(crate) fn doc_field_key(key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<FieldKey> {
    if key != "f9" || ctrl {
        return None;
    }
    match (shift, alt) {
        (false, false) => Some(FieldKey::Update),
        (false, true) => Some(FieldKey::ToggleCodes),
        (true, false) => Some(FieldKey::Edit),
        (true, true) => None,
    }
}

/// A key that inserts a date or time field (#1433).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsertKey {
    /// Alt+Shift+D: a DATE field in the short date.
    Date,
    /// Alt+Shift+T: a TIME field.
    Time,
}

impl InsertKey {
    /// The field's instruction, as Word's Alt+Shift+D / T write it.
    pub(crate) fn instr(self) -> &'static str {
        match self {
            InsertKey::Date => "DATE \\@ \"M/d/yyyy\"",
            InsertKey::Time => "TIME \\@ \"h:mm AM/PM\"",
        }
    }
}

/// The insert key `key` with these modifiers is: Alt+Shift+D and Alt+Shift+T
/// and nothing with Ctrl or Cmd.
pub(crate) fn doc_insert_key(key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<InsertKey> {
    if ctrl || !shift || !alt {
        return None;
    }
    match key {
        "d" => Some(InsertKey::Date),
        "t" => Some(InsertKey::Time),
        _ => None,
    }
}

/// The status when Shift+F9 finds no field to edit.
pub(crate) const NO_FIELD: &str = "There is no field at the insertion point";

/// Edit Field over a field whose code is `instr`: the code, trimmed, in one
/// text field that has the focus, and OK and Cancel.
pub(crate) fn dialog(instr: &str) -> Dialog {
    let mut d = Dialog::message(
        catalog::EDIT_FIELD,
        "Edit Field",
        String::new(),
        &[],
        DialogOwner::EditField,
    );
    d.text = None;
    d.controls = vec![Control::new(
        "code",
        "Field codes:",
        ControlKind::Text,
        Value::Text(instr.trim().to_string()),
    )];
    d.buttons = ok_cancel();
    d.mark_opened();
    d
}

/// A press of `button` on the tab's top dialog when it is Edit Field:
/// `None` for any other dialog or for Cancel (the stack closes it). OK
/// gives the field at the caret the typed code and updates its result, as
/// one undo step; an unchanged code changes nothing, and an empty one is
/// refused with the dialog left open.
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    if top.owner != DialogOwner::EditField {
        return None;
    }
    let want = button.replace('&', "");
    let accept = top
        .buttons
        .iter()
        .find(|b| b.label.replace('&', "").eq_ignore_ascii_case(want.trim()))
        .is_some_and(|b| b.role == ButtonRole::Accept && b.enabled);
    if !accept {
        return None;
    }
    let code = text_of(top, "code").trim().to_string();
    if code.is_empty() {
        return Some(Err("Type a field code".into()));
    }
    let ctx = tab_field_context(Some(tab));
    tab.dialogs.pop();
    let changed = tab_edit_target(tab).is_some_and(|ed| {
        let same = ed
            .field_at_caret()
            .is_some_and(|old| old.trim() == code.as_str());
        // Word writes a field's code between spaces: ` PAGE `.
        !same && ed.set_field_instr(&format!(" {code} "), &ctx)
    });
    if changed {
        tab.mark_dirty();
    }
    Some(Ok(()))
}

impl Docxy {
    /// Run a field key on the active document tab. Protected View and
    /// final documents refuse F9 and Shift+F9 before this (`on_key`).
    pub(crate) fn doc_field_key_press(
        &mut self,
        key: FieldKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ctx = tab_field_context(self.tabs.get(self.active));
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        match key {
            FieldKey::ToggleCodes => tab.field_codes = !tab.field_codes,
            FieldKey::Update => {
                let updated = tab_edit_target(tab).map_or(0, |ed| ed.update_fields(&ctx));
                if updated > 0 {
                    tab.mark_dirty();
                }
            }
            FieldKey::Edit => match tab_edit_target(tab).and_then(|ed| ed.field_at_caret()) {
                Some(instr) => tab.dialogs.push(dialog(&instr)),
                None => self.set_status(NO_FIELD),
            },
        }
        self.refocus(window, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctlcore::json::Json;

    #[test]
    fn alt_shift_d_and_t_are_the_insert_keys_1433() {
        assert_eq!(
            doc_insert_key("d", false, true, true),
            Some(InsertKey::Date)
        );
        assert_eq!(
            doc_insert_key("t", false, true, true),
            Some(InsertKey::Time)
        );
        // Not without Alt or Shift, not with Ctrl or Cmd, not another letter.
        for (key, ctrl, shift, alt) in [
            ("d", false, false, true),
            ("d", false, true, false),
            ("d", true, true, true),
            ("x", false, true, true),
        ] {
            assert_eq!(doc_insert_key(key, ctrl, shift, alt), None, "{key}");
        }
        // The fields they insert show the date and the time.
        let now = docxcore::field::DateTime {
            year: 2026,
            month: 6,
            day: 23,
            hour: 14,
            min: 5,
            sec: 0,
            ..Default::default()
        };
        let ctx = docxcore::field::FieldContext {
            now: Some(now),
            ..Default::default()
        };
        assert_eq!(
            docxcore::field::eval_field_ctx(InsertKey::Date.instr(), &ctx).as_deref(),
            Some("6/23/2026")
        );
        assert_eq!(
            docxcore::field::eval_field_ctx(InsertKey::Time.instr(), &ctx).as_deref(),
            Some("2:05 PM")
        );
    }

    #[test]
    fn f9_alt_f9_and_shift_f9_are_the_field_keys_1241() {
        assert_eq!(
            doc_field_key("f9", false, false, false),
            Some(FieldKey::Update)
        );
        assert_eq!(
            doc_field_key("f9", false, false, true),
            Some(FieldKey::ToggleCodes)
        );
        assert_eq!(
            doc_field_key("f9", false, true, false),
            Some(FieldKey::Edit)
        );
        // Ctrl+F9, Ctrl+Shift+F9, Ctrl+Alt+F9 and Alt+Shift+F9 are not.
        for (ctrl, shift, alt) in [
            (true, false, false),
            (true, true, false),
            (true, false, true),
            (false, true, true),
        ] {
            assert_eq!(doc_field_key("f9", ctrl, shift, alt), None);
        }
        assert_eq!(doc_field_key("f8", false, false, false), None);
    }

    #[test]
    fn edit_field_shows_the_trimmed_code_in_its_one_field_1241() {
        let d = dialog(" PAGE \\* MERGEFORMAT ");
        assert_eq!(d.id, catalog::EDIT_FIELD);
        assert_eq!(d.title, "Edit Field");
        assert_eq!(d.controls.len(), 1);
        assert_eq!(text_of(&d, "code"), "PAGE \\* MERGEFORMAT");
        let mut d = d;
        d.set(
            "code",
            &Json::obj(vec![("value", Json::Str("DATE".into()))]),
        )
        .unwrap();
        assert_eq!(text_of(&d, "code"), "DATE");
    }
}
