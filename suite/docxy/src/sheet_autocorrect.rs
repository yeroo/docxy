//! AutoCorrect on the sheet tab (#667, ENT-112..121): corrections while a
//! text entry is typed and at its commit, Ctrl+Z taking back only the last
//! correction, typed URLs becoming hyperlinks, and the AutoCorrect dialog.
//!
//! The rules are [`gridcore::autocorrect`]'s. The list and switches are an
//! app setting, like the user name: the dialogs sit on the active tab's
//! stack (`dialog-read`/`-set`/`-click` drive them) but their presses go to
//! the app ([`Docxy::autocorrect_click`]), and Add, Delete and the
//! exceptions' edits take effect at once, as Office's do; the check boxes
//! apply with OK. Everything persists in `session.json` with the Sheet
//! editing options. Hyperlinks a typed URL makes live in the workbook model
//! but are not written to the file yet: the xlsx writer keeps the
//! hyperlinks a file had (a follow-up).

use super::*;
use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use gridcore::autocorrect::{AutoCorrect, ExceptionKind, MATH, SWITCHES};
use std::rc::Rc;

/// The switches on each of the dialog's four tabs, by persisted key.
const PAGES: [&[&str]; 4] = [
    &[
        "ac_show_buttons",
        "ac_two_initial_caps",
        "ac_first_letter",
        "ac_names_of_days",
        "ac_caps_lock",
        "ac_replace_text",
    ],
    &["ac_hyperlinks", "ac_table_rows_cols", "ac_table_formulas"],
    &["ac_additional_actions"],
    &["ac_math_outside", "ac_math_replace"],
];

/// How the replace list shows an entry, and reads it back.
const ARROW: &str = " \u{2192} ";

fn entry_item(replace: &str, with: &str) -> String {
    format!("{replace}{ARROW}{with}")
}

impl SheetView {
    /// After `c` was typed into the editor (`at_end`: at the end of its
    /// text) and before AutoComplete proposes: a word it ends is
    /// corrected, and the change kept for Ctrl+Z. The buffer and caret it
    /// leaves are remembered when the typing was at the end, so the commit
    /// corrects only a last word just typed.
    pub(crate) fn autocorrect_typed(&mut self, c: &str, at_end: bool) {
        self.edit_correction = None;
        let Some(buf) = self.editing.clone() else {
            return;
        };
        let ends = c
            .chars()
            .last()
            .is_some_and(gridcore::autocorrect::ends_word);
        if ends {
            let at = self.edit_caret.saturating_sub(1);
            let fix = self
                .autocorrect
                .correct(&buf, at)
                .filter(|fix| self.edit_kept != Some(fix.start));
            if let Some(fix) = fix {
                let text = fix.apply(&buf);
                let caret = self.edit_caret.saturating_add_signed(fix.shift());
                self.editing = Some(text.clone());
                self.edit_caret = caret;
                self.edit_correction = Some((fix, text, caret));
            }
        }
        self.edit_typed_tail = at_end
            .then(|| self.editing.clone().map(|b| (b, self.edit_caret)))
            .flatten();
    }

    /// The commit's AutoCorrect: the last word, when it was just typed
    /// (the buffer and caret are still those the typing left), no
    /// AutoComplete proposal spelled the entry (`took`), and Ctrl+Z did not
    /// take that word back.
    pub(crate) fn autocorrect_commit(&mut self, took: bool) {
        if took {
            return;
        }
        let Some(buf) = self.editing.clone() else {
            return;
        };
        let typed = self
            .edit_typed_tail
            .as_ref()
            .is_some_and(|(b, c)| *b == buf && *c == self.edit_caret);
        if !typed {
            return;
        }
        let fix = self
            .autocorrect
            .correct_at_commit(&buf)
            .filter(|fix| self.edit_kept != Some(fix.start));
        if let Some(fix) = fix {
            self.editing = Some(fix.apply(&buf));
            self.edit_caret_to_end();
        }
    }

    /// Ctrl+Z right after a correction: take back only it (ENT-119) — and
    /// first the AutoComplete proposal the corrected word started. False
    /// when the last change was not a correction.
    pub(crate) fn undo_correction(&mut self) -> bool {
        let Some((fix, after, caret)) = self.edit_correction.take() else {
            return false;
        };
        if self.edit_proposal.is_some() {
            self.drop_proposal();
        }
        if self.editing.as_deref() != Some(after.as_str()) || self.edit_caret != caret {
            return false;
        }
        let Some(text) = fix.undo(&after) else {
            return false;
        };
        self.editing = Some(text);
        self.edit_caret = caret.saturating_add_signed(-fix.shift());
        self.edit_kept = Some(fix.start);
        self.edit_typed_tail = None;
        true
    }

    /// The hyperlink a commit of `buf` makes (ENT-120), when it was typed
    /// (`took`: no proposal spelled it) and commits as text.
    pub(crate) fn typed_link(
        &self,
        buf: &str,
        took: bool,
        cell: Option<&gridcore::sheet::Cell>,
    ) -> Option<String> {
        let text = cell.is_some_and(|c| {
            c.formula.is_none() && matches!(c.value, gridcore::sheet::CellValue::Text(_))
        });
        (!took && text)
            .then(|| self.autocorrect.hyperlink(buf))
            .flatten()
    }
}

/// Every sheet tab's AutoCorrect, the app's: on restore, on a change and
/// each frame, as [`stamp_edit_opts`] does the Editing options.
pub(crate) fn stamp_autocorrect(tabs: &mut [DocTab], ac: &Rc<AutoCorrect>) {
    for t in tabs {
        if let Surface::Sheet(v) = &mut t.surface {
            v.autocorrect = ac.clone();
        }
    }
}

/// The AutoCorrect dialog (ENT-112), on `ac`.
pub(crate) fn dialog(ac: &AutoCorrect) -> Dialog {
    let mut d = Dialog::message(
        "autocorrect",
        "AutoCorrect",
        String::new(),
        &[
            ("Add", ButtonRole::Apply),
            ("Delete", ButtonRole::Apply),
            ("Exceptions...", ButtonRole::Apply),
            ("OK", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::AutoCorrect,
    );
    d.text = None;
    d.buttons = d
        .buttons
        .into_iter()
        .map(|b| Button {
            default: b.label == "OK",
            ..b
        })
        .collect();
    d.tabs = [
        "AutoCorrect",
        "AutoFormat As You Type",
        "Actions",
        "Math AutoCorrect",
    ]
    .map(String::from)
    .to_vec();
    let on_page = |mut c: Control, page: usize| {
        c.page = Some(page);
        c
    };
    for (page, keys) in PAGES.iter().enumerate() {
        for key in *keys {
            let (_, label, _) = SWITCHES
                .iter()
                .find(|(k, _, _)| k == key)
                .expect("a known switch");
            let on = ac.opts.get(key).unwrap_or(false);
            d.controls.push(on_page(
                Control::new(key, label, ControlKind::Checkbox, Value::Bool(on)),
                page,
            ));
        }
    }
    d.controls.push(on_page(
        Control::new(
            "replace",
            "Replace:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
        0,
    ));
    d.controls.push(on_page(
        Control::new(
            "with",
            "With:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
        0,
    ));
    let mut list = Control::new(
        "entries",
        "Entries:",
        ControlKind::List,
        Value::Choice(None),
    );
    list.items = entry_items(ac);
    d.controls.push(on_page(list, 0));
    let mut math = Control::new(
        "math-entries",
        "Math AutoCorrect entries:",
        ControlKind::List,
        Value::Choice(None),
    );
    math.items = MATH.iter().map(|(r, w)| entry_item(r, w)).collect();
    math.enabled = false;
    d.controls.push(on_page(math, 3));
    d.react = Some(crate::dialog::Reaction(picked_entry));
    d.mark_opened();
    d
}

fn entry_items(ac: &AutoCorrect) -> Vec<String> {
    ac.entries().iter().map(|(r, w)| entry_item(r, w)).collect()
}

/// Choosing an entry of the list fills Replace and With with it, as the
/// dialog's list does.
fn picked_entry(d: &mut Dialog, i: usize, _before: &Value) {
    if d.controls[i].name != "entries" {
        return;
    }
    let Value::Choice(Some(k)) = d.controls[i].value else {
        return;
    };
    let Some((r, w)) = d.controls[i]
        .items
        .get(k)
        .and_then(|item| item.split_once(ARROW))
        .map(|(r, w)| (r.to_string(), w.to_string()))
    else {
        return;
    };
    set_text(d, "replace", &r);
    set_text(d, "with", &w);
}

fn set_text(d: &mut Dialog, name: &str, text: &str) {
    if let Some(c) = d.controls.iter_mut().find(|c| c.name == name) {
        c.value = Value::Text(text.to_string());
    }
}

fn text(d: &Dialog, name: &str) -> String {
    match d.controls.iter().find(|c| c.name == name).map(|c| &c.value) {
        Some(Value::Text(t)) => t.trim().to_string(),
        _ => String::new(),
    }
}

fn chosen(d: &Dialog, name: &str) -> Option<String> {
    let c = d.controls.iter().find(|c| c.name == name)?;
    match c.value {
        Value::Choice(Some(k)) => c.items.get(k).cloned(),
        _ => None,
    }
}

/// The AutoCorrect Exceptions dialog (ENT-117).
pub(crate) fn exceptions_dialog(ac: &AutoCorrect) -> Dialog {
    let mut d = Dialog::message(
        "autocorrect-exceptions",
        "AutoCorrect Exceptions",
        String::new(),
        &[
            ("Add", ButtonRole::Apply),
            ("Delete", ButtonRole::Apply),
            ("OK", ButtonRole::Accept),
            ("Close", ButtonRole::Cancel),
        ],
        DialogOwner::AutoCorrectExceptions,
    );
    d.text = None;
    d.tabs = vec!["First Letter".into(), "INitial CAps".into()];
    let pages = [
        (
            "first-word",
            "Don't capitalize after:",
            "first-list",
            ExceptionKind::FirstLetter,
        ),
        (
            "caps-word",
            "Don't correct:",
            "caps-list",
            ExceptionKind::InitialCaps,
        ),
    ];
    for (page, (word, label, list, kind)) in pages.into_iter().enumerate() {
        let mut field = Control::new(word, label, ControlKind::Text, Value::Text(String::new()));
        field.page = Some(page);
        let mut items = Control::new(list, "Exceptions:", ControlKind::List, Value::Choice(None));
        items.items = ac.exceptions(kind);
        items.page = Some(page);
        d.controls.push(field);
        d.controls.push(items);
    }
    d.mark_opened();
    d
}

/// The question before Add replaces an existing entry (ENT-116).
fn redefine_dialog(replace: &str) -> Dialog {
    Dialog::message(
        "autocorrect-redefine",
        "AutoCorrect",
        format!("The AutoCorrect entry \"{replace}\" already exists. Do you want to redefine it?"),
        &[("Yes", ButtonRole::Accept), ("No", ButtonRole::Cancel)],
        DialogOwner::AutoCorrectRedefine,
    )
}

/// Whether `owner` is one of AutoCorrect's dialogs, whose presses are the
/// app's.
pub(crate) fn is_autocorrect(owner: DialogOwner) -> bool {
    matches!(
        owner,
        DialogOwner::AutoCorrect
            | DialogOwner::AutoCorrectExceptions
            | DialogOwner::AutoCorrectRedefine
    )
}

fn presses(button: &str, label: &str) -> bool {
    button.replace('&', "").trim().eq_ignore_ascii_case(label)
}

/// A press on an AutoCorrect dialog, given the dialogs and the app's
/// AutoCorrect; what it changed in `ac` is the caller's to store. `None`
/// when the top dialog is not one of these.
pub(crate) fn click(
    dialogs: &mut crate::dialog::DialogStack,
    ac: &mut AutoCorrect,
    button: &str,
) -> Option<Result<bool, String>> {
    let owner = dialogs.top()?.owner;
    if !is_autocorrect(owner) {
        return None;
    }
    Some(match owner {
        DialogOwner::AutoCorrect => main_click(dialogs, ac, button),
        DialogOwner::AutoCorrectExceptions => exceptions_click(dialogs, ac, button),
        _ => redefine_click(dialogs, ac, button),
    })
}

/// The AutoCorrect tab's Add, Delete and Exceptions...; OK takes the
/// switches. `Ok(true)` when `ac` changed.
fn main_click(
    dialogs: &mut crate::dialog::DialogStack,
    ac: &mut AutoCorrect,
    button: &str,
) -> Result<bool, String> {
    let top = dialogs.top_dialog_mut()?;
    if presses(button, "Add") {
        let (replace, with) = (text(top, "replace"), text(top, "with"));
        if replace.is_empty() {
            return Err("Type the text to replace in Replace".into());
        }
        if ac.lookup(&replace).is_some_and(|old| old != with) {
            dialogs.push(redefine_dialog(&replace));
            return Ok(false);
        }
        ac.add(&replace, &with).map_err(|e| e.to_string())?;
        refresh(top, ac);
        return Ok(true);
    }
    if presses(button, "Delete") {
        let replace = chosen(top, "entries")
            .and_then(|i| i.split_once(ARROW).map(|(r, _)| r.to_string()))
            .unwrap_or_else(|| text(top, "replace"));
        if !ac.delete(&replace) {
            return Err(format!("'{replace}' is not in the AutoCorrect list"));
        }
        refresh(top, ac);
        return Ok(true);
    }
    if presses(button, "Exceptions...") {
        let d = exceptions_dialog(ac);
        dialogs.push(d);
        return Ok(false);
    }
    let mut changed = false;
    dialogs.click(button, |d| {
        for keys in PAGES {
            for key in keys {
                let on = d
                    .controls
                    .iter()
                    .find(|c| c.name == *key)
                    .is_some_and(|c| c.value == Value::Bool(true));
                if let Some(slot) = ac.opts.slot(key) {
                    changed |= *slot != on;
                    *slot = on;
                }
            }
        }
        Ok(())
    })?;
    Ok(changed)
}

/// The list and fields after Add or Delete: the list as it now is, the
/// fields empty.
fn refresh(d: &mut Dialog, ac: &AutoCorrect) {
    if let Some(list) = d.controls.iter_mut().find(|c| c.name == "entries") {
        list.items = entry_items(ac);
        list.value = Value::Choice(None);
    }
    set_text(d, "replace", "");
    set_text(d, "with", "");
}

/// Yes on "Do you want to redefine it?": the entry the dialog under it
/// names is replaced. No just closes.
fn redefine_click(
    dialogs: &mut crate::dialog::DialogStack,
    ac: &mut AutoCorrect,
    button: &str,
) -> Result<bool, String> {
    let yes = presses(button, "Yes");
    dialogs.click(button, |_| Ok(()))?;
    if !yes {
        return Ok(false);
    }
    let Ok(parent) = dialogs.top_dialog_mut() else {
        return Ok(false);
    };
    let (replace, with) = (text(parent, "replace"), text(parent, "with"));
    ac.add(&replace, &with).map_err(|e| e.to_string())?;
    refresh(parent, ac);
    Ok(true)
}

/// The Exceptions dialog's Add and Delete, on the tab it shows; OK and
/// Close close it.
fn exceptions_click(
    dialogs: &mut crate::dialog::DialogStack,
    ac: &mut AutoCorrect,
    button: &str,
) -> Result<bool, String> {
    let top = dialogs.top_dialog_mut()?;
    let (word, list, kind) = if top.tab == 0 {
        ("first-word", "first-list", ExceptionKind::FirstLetter)
    } else {
        ("caps-word", "caps-list", ExceptionKind::InitialCaps)
    };
    if presses(button, "Add") {
        let w = text(top, word);
        if w.is_empty() {
            return Err("Type the word to add".into());
        }
        ac.add_exception(kind, &w).map_err(|e| e.to_string())?;
    } else if presses(button, "Delete") {
        let w = chosen(top, list).unwrap_or_else(|| text(top, word));
        if !ac.delete_exception(kind, &w) {
            return Err(format!("'{w}' is not an exception"));
        }
    } else {
        dialogs.click(button, |_| Ok(()))?;
        return Ok(false);
    }
    if let Some(c) = top.controls.iter_mut().find(|c| c.name == list) {
        c.items = ac.exceptions(kind);
        c.value = Value::Choice(None);
    }
    set_text(top, word, "");
    Ok(true)
}

impl Docxy {
    /// Settings' AutoCorrect Options... (and the harness's `autocorrect`):
    /// the dialog on the active tab.
    pub(crate) fn open_autocorrect_dialog(&mut self) -> Result<(), String> {
        let d = dialog(&self.autocorrect);
        let tab = self
            .tabs
            .get_mut(self.active)
            .ok_or(crate::dialog::NONE_OPEN)?;
        tab.dialogs.push(d);
        Ok(())
    }

    /// [`click`] on the active tab, storing and persisting what changed.
    pub(crate) fn autocorrect_click(&mut self, button: &str) -> Option<Result<(), String>> {
        let tab = self.tabs.get_mut(self.active)?;
        let mut ac = (*self.autocorrect).clone();
        let done = click(&mut tab.dialogs, &mut ac, button)?;
        Some(done.map(|changed| {
            if changed {
                self.autocorrect = Rc::new(ac);
                stamp_autocorrect(&mut self.tabs, &self.autocorrect);
                self.persist();
            }
        }))
    }
}

#[cfg(test)]
mod tests;
