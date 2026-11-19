//! Further extension tab kinds (#2253): the kinds a build registers through
//! [`ext::Extensions::kinds`], beside Mail.
//!
//! A tab of one is `Kind::Ext(ExtId)`. [`ExtId`] is the kind's id, interned
//! once per distinct id for the life of the process, so `Kind` stays `Copy`
//! and needs no lock to compare. That interner is the only process-wide
//! state: which kinds are registered, with their cards, icons, accents and
//! factories, lives in the gpui [`ext::Installed`] global, per App, so
//! parallel gpui tests never see each other's kinds. A tab of an id this
//! build does not register (a session written by another build) is an
//! `ExtId` too, with nothing registered for it.

use gpui::App;

use crate::ext::{self, Factory};

/// The id of an extension tab kind (#2253), interned: equal ids are equal.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) struct ExtId(&'static str);

impl ExtId {
    pub(crate) fn intern(id: &str) -> Self {
        ExtId(crate::plugin_ui::intern(id))
    }
    pub(crate) fn as_str(self) -> &'static str {
        self.0
    }
}

/// A registered kind as the suite keeps it: [`ext::ExtKind`] with its icon
/// registered as an asset (`ext/<hash>`, see `ext_ribbon::register_icon`).
pub(crate) struct Registered {
    pub(crate) id: ExtId,
    pub(crate) name: String,
    /// The title bar's one-word type label: the name, lowercased.
    pub(crate) label: &'static str,
    pub(crate) sub: String,
    pub(crate) title: String,
    pub(crate) icon: Option<&'static str>,
    pub(crate) accent: Option<(u32, u32)>,
    pub(crate) factory: Factory,
}

/// The ids the suite's own kinds use, in the session (`look` is Mail's) or
/// in the harness (`mail`): a further kind may take none of them.
const RESERVED: [&str; 7] = ["docx", "xlsx", "project", "mail", "look", "help", "pdf"];

/// Why `id` cannot name a kind, if it cannot.
fn bad_id(id: &str) -> Option<&'static str> {
    if id.is_empty() {
        return Some("its id is empty");
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Some("its id is not lowercase ASCII letters, digits, '.', '_' or '-'");
    }
    if RESERVED.contains(&id) {
        return Some("its id is one of the suite's own kinds");
    }
    None
}

/// The kinds `install` keeps, in order, and one line for each it leaves out:
/// a bad id ([`bad_id`]) or one an earlier kind already has.
pub(crate) fn validate(kinds: Vec<ext::ExtKind>) -> (Vec<Registered>, Vec<String>) {
    let mut kept: Vec<Registered> = Vec::new();
    let mut errors = Vec::new();
    for k in kinds {
        let why = bad_id(&k.id).or_else(|| {
            kept.iter()
                .any(|r| r.id.as_str() == k.id)
                .then_some("an earlier kind has its id")
        });
        if let Some(why) = why {
            errors.push(format!("extension tab kind '{}' not loaded: {why}", k.id));
            continue;
        }
        kept.push(Registered {
            id: ExtId::intern(&k.id),
            label: crate::plugin_ui::intern(&k.name.to_lowercase()),
            name: k.name,
            sub: k.sub,
            title: k.title,
            icon: k.icon.as_ref().map(crate::ext_ribbon::register_icon),
            accent: k.accent,
            factory: k.factory,
        });
    }
    (kept, errors)
}

/// The kinds this App registers, in order (none before `install`).
pub(crate) fn registered(cx: &App) -> &[Registered] {
    cx.try_global::<ext::Installed>()
        .map_or(&[], |i| i.kinds.as_slice())
}

/// The registered kind `id`, if this App has one.
pub(crate) fn find(id: ExtId, cx: &App) -> Option<&Registered> {
    registered(cx).iter().find(|r| r.id == id)
}

/// The accent of kind `kind_id`'s tabs, light and dark, if this App
/// registers it with one.
pub(crate) fn accent_of(kind_id: &str, cx: &App) -> Option<(u32, u32)> {
    registered(cx)
        .iter()
        .find(|r| r.id.as_str() == kind_id)
        .and_then(|r| r.accent)
}

/// The status line of a restored tab of a kind this build does not register.
pub(crate) fn unavailable_status(id: ExtId) -> String {
    format!("{}: not available in this build", id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn kind(id: &str) -> ext::ExtKind {
        let factory: Factory = Rc::new(|_| Err("no".into()));
        ext::ExtKind::new(id, "Name", "Sub", "Title", factory)
    }

    #[test]
    fn bad_built_in_and_repeated_ids_are_left_out_and_said() {
        let (kept, errors) = validate(vec![
            kind("notes"),
            kind(""),
            kind("Notes"),
            kind("a b"),
            kind("mail"),
            kind("look"),
            kind("docx"),
            kind("notes"),
            kind("com.example_x-1"),
        ]);
        let ids: Vec<&str> = kept.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["notes", "com.example_x-1"]);
        assert_eq!(errors.len(), 7, "{errors:?}");
        assert!(errors[0].contains("''") && errors[0].contains("empty"));
        assert!(errors[1].contains("'Notes'"));
        assert!(errors[3].contains("suite's own"));
        assert!(errors[6].contains("earlier kind"));
    }

    #[test]
    fn ids_intern_by_content() {
        let a = ExtId::intern(&String::from("same.kind"));
        let b = ExtId::intern("same.kind");
        assert_eq!(a, b);
        assert!(std::ptr::eq(a.as_str(), b.as_str()));
        assert_ne!(a, ExtId::intern("other.kind"));
    }
}
