//! Settings' User name... (#620): Word's "Personalize your copy" name and
//! initials, which new comments are stamped with.
//!
//! A form on the active tab's dialog stack, so `dialog-set` and
//! `dialog-click` drive it like any other. Its OK is the app's, not the
//! tab's: [`click`] takes the press before the tab's own path does, and the
//! app stores and persists what it returns.
use crate::dialog::{Control as Field, ControlKind, Dialog, DialogOwner, DialogStack, Value};
use crate::page_setup::{ok_cancel, text_of};
use crate::{Docxy, review_identity};

/// The dialog, its fields filled with the name and initials comments carry
/// now ([`review_identity`]), as Word fills them.
pub(crate) fn dialog(user_name: &str, user_initials: &str) -> Dialog {
    let (name, initials) = review_identity(user_name, user_initials);
    let mut d = Dialog::message(
        "user-name",
        "User name",
        String::new(),
        &[],
        DialogOwner::UserName,
    );
    d.text = None;
    d.controls = vec![
        Field::new(
            "user-name",
            "User name:",
            ControlKind::Text,
            Value::Text(name),
        ),
        Field::new(
            "initials",
            "Initials:",
            ControlKind::Text,
            Value::Text(initials),
        ),
    ];
    d.buttons = ok_cancel();
    d.mark_opened();
    // The first field has the focus, so typing edits it at once (#1027).
    d.focus_step(false);
    d
}

/// What an OK hands back: the user name and initials to store.
pub(crate) type Identity = (String, String);

/// A press of `button` on `dialogs` when the dialog on top is this one:
/// `None` for any other dialog. Cancel closes it; OK closes it and hands
/// back the trimmed name and initials to store.
pub(crate) fn click(
    dialogs: &mut DialogStack,
    button: &str,
) -> Option<(Result<(), String>, Option<Identity>)> {
    if dialogs.top()?.owner != DialogOwner::UserName {
        return None;
    }
    let mut accepted = None;
    let done = dialogs.click(button, |d| {
        accepted = Some((
            text_of(d, "user-name").trim().to_string(),
            text_of(d, "initials").trim().to_string(),
        ));
        Ok(())
    });
    Some((done, accepted))
}

impl Docxy {
    /// Settings' User name... row: open the dialog on the active tab's
    /// stack, or, with no document open, on the app's own (#1027).
    pub(crate) fn open_user_name_dialog(&mut self) -> Result<(), String> {
        let d = dialog(&self.user_name, &self.user_initials);
        self.active_dialogs_mut().push(d);
        Ok(())
    }

    /// [`click`] on the active stack, storing and persisting an OK's values.
    pub(crate) fn user_name_click(&mut self, button: &str) -> Option<Result<(), String>> {
        let (done, accepted) = click(self.active_dialogs_mut(), button)?;
        if let Some((name, initials)) = accepted {
            self.user_name = name;
            self.user_initials = initials;
            crate::set_configured_identity(&self.user_name, &self.user_initials);
            crate::reauthor_tracking(&mut self.tabs, &self.user_name, &self.user_initials);
            self.persist();
        }
        Some(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctlcore::json::Json;
    use gpui::Modifiers;

    /// `dialog-set`'s arguments for a text field.
    fn value(text: &str) -> Json {
        Json::obj(vec![("value", Json::Str(text.into()))])
    }

    fn opened(name: &str, initials: &str) -> DialogStack {
        let mut stack = DialogStack::default();
        stack.push(dialog(name, initials));
        stack
    }

    #[test]
    fn ok_hands_back_the_typed_name_and_initials() {
        let mut stack = opened("", "");
        stack.set("user-name", &value("  Jane Doe ")).unwrap();
        stack.set("initials", &value("jd")).unwrap();
        let (done, accepted) = click(&mut stack, "OK").expect("ours");
        done.unwrap();
        assert_eq!(accepted, Some(("Jane Doe".into(), "jd".into())));
        assert!(!stack.is_open());
    }

    #[test]
    fn cancel_closes_and_keeps_nothing() {
        let mut stack = opened("Jane Doe", "JD");
        stack.set("user-name", &value("Other")).unwrap();
        let (done, accepted) = click(&mut stack, "Cancel").expect("ours");
        done.unwrap();
        assert_eq!(accepted, None);
        assert!(!stack.is_open());
    }

    #[test]
    fn fields_open_with_the_configured_identity() {
        let d = dialog("Jane Doe", "");
        assert_eq!(text_of(&d, "user-name"), "Jane Doe");
        assert_eq!(text_of(&d, "initials"), "JD", "derived when not set");
        let d = dialog("Jane Doe", "Jx");
        assert_eq!(text_of(&d, "initials"), "Jx");
    }

    #[test]
    fn another_dialog_is_not_ours() {
        let mut stack = DialogStack::default();
        stack.push(Dialog::message(
            "t",
            "T",
            String::new(),
            &[("OK", crate::dialog::ButtonRole::Accept)],
            DialogOwner::Test,
        ));
        assert!(click(&mut stack, "OK").is_none());
        assert!(stack.is_open());
    }

    #[test]
    fn the_first_field_has_the_focus_and_typing_edits_it_in_place() {
        let mut stack = opened("Jane", "J");
        assert_eq!(
            stack.top().unwrap().focused().map(|c| c.name),
            Some("user-name")
        );
        let none = Modifiers::default();
        let ctrl = Modifiers {
            control: true,
            ..none
        };
        crate::dialog_host::edit_key(&mut stack, "a", None, ctrl, None).unwrap();
        for c in ["D", "o", "e"] {
            crate::dialog_host::edit_key(&mut stack, c, Some(c), none, None).unwrap();
        }
        crate::dialog_host::edit_key(&mut stack, "tab", None, none, None).unwrap();
        crate::dialog_host::edit_key(&mut stack, "v", None, ctrl, Some("DOE")).unwrap();
        // The second field was "J": the paste goes after it.
        let (done, accepted) = click(&mut stack, "OK").expect("ours");
        done.unwrap();
        assert_eq!(accepted, Some(("Doe".into(), "JDOE".into())));
    }
}
