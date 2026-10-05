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
    /// Settings' User name... row: open the dialog on the active tab.
    pub(crate) fn open_user_name_dialog(&mut self) -> Result<(), String> {
        let d = dialog(&self.user_name, &self.user_initials);
        let tab = self
            .tabs
            .get_mut(self.active)
            .ok_or(crate::dialog::NONE_OPEN)?;
        tab.dialogs.push(d);
        Ok(())
    }

    /// [`click`] on the active tab, storing and persisting an OK's values.
    pub(crate) fn user_name_click(&mut self, button: &str) -> Option<Result<(), String>> {
        let tab = self.tabs.get_mut(self.active)?;
        let (done, accepted) = click(&mut tab.dialogs, button)?;
        if let Some((name, initials)) = accepted {
            self.user_name = name;
            self.user_initials = initials;
            crate::set_configured_identity(&self.user_name, &self.user_initials);
            // Tabs already recording keep recording, as the new reviewer.
            let author = crate::track_author(&crate::review_identity(
                &self.user_name,
                &self.user_initials,
            ));
            for tab in &mut self.tabs {
                if let crate::Surface::Doc(ed) = &mut tab.surface {
                    if ed.track_changes() {
                        ed.set_track_changes(Some(author.clone()));
                    }
                }
            }
            self.persist();
        }
        Some(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctlcore::json::Json;

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
}
