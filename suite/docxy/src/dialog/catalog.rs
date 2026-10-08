//! Every dialog the suite builds, how a person opens it, and the fields they
//! type into (#1029).
//!
//! A [`DialogId`] can only be made here: its fields are private to this
//! module, and `Dialog::message` (the one constructor) takes one. So every
//! dialog carries a catalogued id; a new dialog that reuses another's id is
//! caught by review, and by `dialog-catalog-check` where a case opens it. The
//! `inputs-typing-*.uit` cases,
//! generated from [`catalog`], type into every field it lists with real keys.
//! The harness's `dialog-catalog-check` holds an open dialog's editable
//! controls to its entry, so a field added to a dialog without one fails the
//! case that opens it.

use super::ControlKind;

/// A dialog's stable id. `id` is what scripts see (`dialog-read`, `assert
/// dialog is`); two dialogs may share it (Word's and Excel's Page Setup are
/// both `page-setup`), so `key`, the constant's name, tells their entries
/// apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DialogId {
    id: &'static str,
    key: &'static str,
}

impl DialogId {
    pub fn as_str(self) -> &'static str {
        self.id
    }

    /// The catalogue key: the constant's name (`SHEET_PAGE_SETUP`).
    pub fn key(self) -> &'static str {
        self.key
    }

    /// Its catalogue entry; `None` only for a test-only id.
    pub fn entry(self) -> Option<&'static Entry> {
        CATALOG.iter().find(|e| e.dialog == self)
    }
}

impl PartialEq<&str> for DialogId {
    fn eq(&self, other: &&str) -> bool {
        self.id == *other
    }
}

/// Where a dialog lives: the tab kind a case opens to reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Surface {
    Doc,
    Sheet,
    Project,
}

impl Surface {
    pub fn name(self) -> &'static str {
        match self {
            Self::Doc => "doc",
            Self::Sheet => "sheet",
            Self::Project => "project",
        }
    }
}

/// How a field refuses a value of the wrong kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refuse {
    /// A number field refuses the character as it is typed: the text stays
    /// as it was.
    AtType,
    /// A date or duration field takes any text; the owner refuses it on OK
    /// and the dialog stays open.
    AtOk,
}

/// One editable control of a dialog.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Field {
    /// The control's name (`Control::name`).
    pub name: &'static str,
    pub kind: ControlKind,
    /// The tab label it sits on, for a dialog with tabs; `None` on every tab
    /// or in a dialog without them.
    pub tab: Option<&'static str>,
    /// Script lines that make it shown and enabled after the dialog opens
    /// (a choice another control gates it on), real input only. They run
    /// before its tab is clicked, on the tab the dialog opens on.
    pub prep: &'static [&'static str],
    /// A valid value of its kind, typed by the cases.
    pub sample: &'static str,
    /// A value of the wrong kind, and how it is refused (number, date and
    /// duration fields).
    pub invalid: Option<(&'static str, Refuse)>,
    /// Assertions that hold once the dialog was accepted with `sample` typed:
    /// the setting, document or sheet changed.
    pub applied: &'static [&'static str],
    /// What the field shows when the dialog is opened again after that OK,
    /// when it reopens on what OK applied.
    pub kept: Option<&'static str>,
    /// Why the cases do not press OK with `sample` typed (it needs other
    /// fields, or nothing reads it back); `None` when they do.
    pub skip_ok: Option<&'static str>,
    /// Why the cases do not type into it at all (a hidden control); `None`
    /// when they do.
    pub skip: Option<&'static str>,
}

impl Field {
    /// A text field with the default samples, typed and accepted.
    pub const fn text(name: &'static str) -> Self {
        Self {
            name,
            kind: ControlKind::Text,
            tab: None,
            prep: &[],
            sample: TEXT_SAMPLE,
            invalid: None,
            applied: &[],
            kept: None,
            skip_ok: None,
            skip: None,
        }
    }

    /// A number field: `sample` typed, a letter refused as it is typed.
    pub const fn number(name: &'static str, sample: &'static str) -> Self {
        Self {
            kind: ControlKind::Number,
            sample,
            invalid: Some(("x", Refuse::AtType)),
            ..Self::text(name)
        }
    }

    pub const fn date(name: &'static str, sample: &'static str, bad: &'static str) -> Self {
        Self {
            kind: ControlKind::Date,
            sample,
            invalid: Some((bad, Refuse::AtOk)),
            ..Self::text(name)
        }
    }

    pub const fn duration(name: &'static str, sample: &'static str, bad: &'static str) -> Self {
        Self {
            kind: ControlKind::Duration,
            sample,
            invalid: Some((bad, Refuse::AtOk)),
            ..Self::text(name)
        }
    }

    /// A control with no typing: a dropdown, checkbox, radio group or check
    /// list. Only dropdowns get cases (a click focuses, Down steps it).
    pub const fn other(name: &'static str, kind: ControlKind) -> Self {
        Self {
            kind,
            sample: "",
            ..Self::text(name)
        }
    }

    pub const fn on(self, tab: &'static str) -> Self {
        Self {
            tab: Some(tab),
            ..self
        }
    }

    pub const fn prep(self, prep: &'static [&'static str]) -> Self {
        Self { prep, ..self }
    }

    pub const fn applied(self, applied: &'static [&'static str]) -> Self {
        Self { applied, ..self }
    }

    pub const fn kept(self, kept: &'static str) -> Self {
        Self {
            kept: Some(kept),
            ..self
        }
    }

    pub const fn no_ok(self, why: &'static str) -> Self {
        Self {
            skip_ok: Some(why),
            ..self
        }
    }

    pub const fn skip(self, why: &'static str) -> Self {
        Self {
            skip: Some(why),
            ..self
        }
    }

    pub const fn sample(self, sample: &'static str) -> Self {
        Self { sample, ..self }
    }

    /// Typed into by the cases: a text, number, date or duration field that
    /// is not skipped.
    pub fn typed(&self) -> bool {
        self.kind.is_text() && self.skip.is_none()
    }
}

/// What the cases type into a text field: a capital, a lower-case letter, a
/// digit and a space, so a field that drops any of them is caught.
pub(crate) const TEXT_SAMPLE: &str = "Ab1 ";

/// One dialog.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Entry {
    pub dialog: DialogId,
    pub surface: Surface,
    /// Its case file: `inputs-typing-<file>.uit`. A file holds a few
    /// related dialogs, so each runs well inside the sweep's per-script
    /// timeout and keeps its name when another dialog is added.
    pub file: &'static str,
    /// The fixture a case opens, relative to `uiharness/cases/`.
    pub fixture: &'static str,
    /// Script lines after the fixture is open that open the dialog the way a
    /// person does (from Backstage where that is its entry point).
    pub open: &'static [&'static str],
    /// Its editable controls on every tab, in the dialog's order. A message
    /// box has none.
    pub fields: &'static [Field],
    /// How a case opens it again over the document as it is, when that
    /// differs from `open` (a child dialog, reopened from its parent);
    /// empty means `open`.
    pub reopen: &'static [&'static str],
    /// Its default button closes it (OK); `false` when it keeps it open
    /// (Find Next).
    pub accept_closes: bool,
    /// Why no case can open it, when none can; the catalogue still lists it.
    pub unreachable: Option<&'static str>,
}

/// The catalogue.
pub(crate) fn catalog() -> &'static [Entry] {
    CATALOG
}

/// Declares each dialog's [`DialogId`] constant together with its entry, so
/// neither can exist without the other.
macro_rules! dialogs {
    ($($konst:ident = $id:literal { $($body:tt)* })*) => {
        $(pub(crate) const $konst: DialogId = DialogId { id: $id, key: stringify!($konst) };)*
        pub(crate) static CATALOG: &[Entry] = &[$(Entry { dialog: $konst, $($body)* },)*];
    };
}

/// Ids only the unit tests build; never in the catalogue or a case.
#[cfg(test)]
pub(crate) const TEST_FORM: DialogId = DialogId {
    id: "form",
    key: "TEST_FORM",
};
#[cfg(test)]
pub(crate) const TEST_CHILD: DialogId = DialogId {
    id: "child",
    key: "TEST_CHILD",
};
#[cfg(test)]
pub(crate) const TEST_T: DialogId = DialogId {
    id: "t",
    key: "TEST_T",
};

mod entries;
pub(crate) use entries::*;

#[cfg(test)]
pub(crate) mod cases;

#[cfg(test)]
mod tests;
