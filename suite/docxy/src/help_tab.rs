//! The Help tab every ribbon ends with (#1021), as Word, Excel and Project
//! end theirs: a Help group (Help, Contact Support, Feedback, Show Training,
//! What's New) and our About group.
//!
//! Help and F1 open the editor's manual in a Help tab (#1855,
//! `help_view`) when one is installed (`help_manual`), else say the
//! documentation is coming: Help on its own page (the `help` key), F1 on the
//! active ribbon tab's area page, or on the page of the open dialog
//! (`dialog:<id>`). Contact
//! Support and Feedback open the docxy GitHub new-issue page with the build
//! pre-filled ([`buildinfo::BuildInfo::feedback_url`]); under the harness the
//! URL is recorded, not opened, and `last-url` reads it. About docxy suite opens
//! the File › Account About dialog (#1023). Show Training and What's New have
//! nothing to show yet: they are drawn disabled.
use super::*;
use crate::help_manual::Editor as HelpEditor;

/// A Help tab command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HelpAct {
    /// Help (F1): the documentation, once it lands.
    Help,
    ContactSupport,
    Feedback,
    /// Disabled: no training content yet.
    ShowTraining,
    /// Disabled: no release notes in the app yet.
    WhatsNew,
    /// About docxy suite: the build info dialog.
    About,
}

/// What Help and F1 say when no manual is installed for the editor.
pub(crate) const DOCS_COMING: &str = "Help: the documentation is coming soon";

/// The status line after Contact Support or Feedback.
pub(crate) const FEEDBACK_OPENED: &str = "Opened the feedback page in your browser";

/// The ScreenTip of the commands that have nothing to show yet.
const COMING_LATER: &str = "Coming later";

/// Whether a Help command does anything yet.
pub(crate) fn help_enabled(act: HelpAct) -> bool {
    !matches!(act, HelpAct::ShowTraining | HelpAct::WhatsNew)
}

fn cmd(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    act: HelpAct,
    tip: &'static str,
    shortcut: &'static str,
) -> rs::Cmd<Act> {
    rs::cmd(id, icon, label, Act::Help(act)).tip(label, tip, shortcut)
}

/// The Help tab of the document and Project ribbons (a workbook draws the same
/// commands from `sheet_ribbon`).
pub(crate) fn help_tab() -> rs::Tab<Act> {
    use HelpAct as H;
    rs::tab(
        "Help",
        "Y",
        vec![
            rs::group(
                "Help",
                20,
                vec![
                    Control::Large(cmd("help", "help-circle", "Help", H::Help, "", "F1").key("H")),
                    rs::column(vec![
                        cmd(
                            "contactsupport",
                            "support",
                            "Contact Support",
                            H::ContactSupport,
                            "",
                            "",
                        )
                        .key("C"),
                        cmd("feedback", "comment", "Feedback", H::Feedback, "", "").key("K"),
                        cmd(
                            "showtraining",
                            "video",
                            "Show Training",
                            H::ShowTraining,
                            COMING_LATER,
                            "",
                        )
                        .key("T"),
                    ]),
                    rs::column(vec![
                        cmd(
                            "whatsnew",
                            "sparkle",
                            "What's New",
                            H::WhatsNew,
                            COMING_LATER,
                            "",
                        )
                        .key("W"),
                    ]),
                ],
            ),
            rs::group(
                "About",
                10,
                vec![Control::Large(
                    cmd("about", "info", "About docxy suite", H::About, "", "").key("A"),
                )],
            ),
        ],
    )
}

/// F1, with no modifier: Help › Help.
pub(crate) fn is_help_key(k: &Keystroke) -> bool {
    is_help_stroke(&k.key, k.modifiers)
}

/// [`is_help_key`] for a key already split into its name and modifiers.
pub(crate) fn is_help_stroke(key: &str, m: Modifiers) -> bool {
    key == "f1" && !(m.control || m.platform || m.alt || m.shift || m.function)
}

/// What a Help command or F1 asks the manual for (#1855).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HelpAsk {
    pub editor: HelpEditor,
    /// A command or dialog id (`bold`, `dialog:font`); `None` asks for the
    /// area's own page, never a key's.
    pub id: Option<String>,
    /// The ribbon tab's name (`Home`), or `None`: the manual's root.
    pub area: Option<String>,
}

impl HelpAsk {
    /// The page of `self` in `manual`, and how it was found.
    pub(crate) fn resolve(&self, manual: &helpdocs::Manual) -> Option<helpdocs::Target> {
        match &self.id {
            Some(id) => helpdocs::resolve(manual, id, self.area.as_deref()),
            None => helpdocs::resolve_area(manual, self.area.as_deref()),
        }
    }
}

/// F1 with no dialog: on a Help tab its manual's root page, else the area
/// page of the ribbon tab shown (`area`) in the manual of the tab's kind.
pub(crate) fn f1_ask(kind: Option<Kind>, area: &str, help_tab: Option<HelpEditor>) -> HelpAsk {
    match help_tab {
        Some(editor) => HelpAsk {
            editor,
            id: None,
            area: None,
        },
        None => HelpAsk {
            editor: help_view::host::editor_for(kind),
            id: None,
            area: Some(area.to_string()),
        },
    }
}

/// Help › Help (the ribbon's, the sheet's, the macOS menu's): the `help`
/// command's page.
pub(crate) fn help_command_ask(kind: Option<Kind>) -> HelpAsk {
    HelpAsk {
        editor: help_view::host::editor_for(kind),
        id: Some("help".into()),
        area: Some("Help".into()),
    }
}

/// F1, or a Help button, on the dialog `id` over a tab of `kind`, with the
/// ribbon tab `area` shown.
pub(crate) fn dialog_ask(kind: Option<Kind>, id: &str, area: &str) -> HelpAsk {
    HelpAsk {
        editor: help_view::host::editor_for(kind),
        id: Some(format!("dialog:{id}")),
        area: Some(area.to_string()),
    }
}

/// Whether F1 over the dialog `d` opens its Help page (#1855): only over a
/// plain form, never a message box, a popup (the AutoFilter's menu), a
/// reload or recovery prompt, or the close and quit prompts (`quitting`:
/// a window close is asking about unsaved tabs), which a tab switch would
/// strand. F1 over any other is swallowed, as before.
pub(crate) fn dialog_takes_help(d: &crate::dialog::Dialog, quitting: bool) -> bool {
    use crate::dialog::DialogOwner as O;
    !quitting
        && d.text.is_none()
        && d.popup.is_none()
        && !matches!(
            d.owner,
            O::Reopen { .. }
                | O::SaveOnClose { .. }
                | O::Message { .. }
                | O::DocumentRecovery { .. }
                | O::DropReplace { .. }
                | O::FilterMenu { .. }
                | O::ProjectFilter { .. }
                | O::UserName { .. }
                | O::About { .. }
        )
}

impl Docxy {
    /// Run a Help tab command.
    pub(crate) fn help_act(&mut self, act: HelpAct, window: &mut Window, cx: &mut Context<Self>) {
        // A disabled command (reached by its KeyTip) does nothing, and leaves
        // the status line as it was.
        if !help_enabled(act) {
            return self.refocus(window, cx);
        }
        match act {
            HelpAct::Help => {
                let kind = self.tabs.get(self.active).map(|t| t.kind);
                if self.open_help(help_command_ask(kind), window, cx) {
                    return;
                }
                self.set_status(DOCS_COMING);
            }
            HelpAct::ContactSupport | HelpAct::Feedback => {
                let url = about::info().feedback_url("suite");
                self.open_help_url(url, cx);
                self.set_status(FEEDBACK_OPENED);
            }
            HelpAct::ShowTraining | HelpAct::WhatsNew => {}
            HelpAct::About => {
                if let Err(e) = self.open_about() {
                    self.set_status(e);
                }
            }
        }
        self.refocus(window, cx);
    }

    /// Open the page `ask` resolves to in a Help tab. `false` (nothing done)
    /// when no manual is installed for its editor.
    pub(crate) fn open_help(
        &mut self,
        ask: HelpAsk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(bundle) = self.help_bundle(ask.editor) else {
            return false;
        };
        let Some(target) = ask.resolve(&bundle.manual) else {
            return false;
        };
        let at = help_view::Loc::page(&target.page);
        self.open_help_at(bundle, at, Some(target.via), window, cx);
        true
    }

    /// F1 with no dialog and no entry owning the keys: the area page of the
    /// ribbon tab shown, or the root of the Help tab's manual; with no manual
    /// installed the status line says the documentation is coming.
    pub(crate) fn help_f1(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keytips = KeyTip::Off;
        let kind = self.tabs.get(self.active).map(|t| t.kind);
        let ask = f1_ask(
            kind,
            crate::ribbon_tab_name(self.ribbon_tab),
            self.active_help().map(help_view::HelpView::editor),
        );
        if !self.open_help(ask, window, cx) {
            self.set_status(DOCS_COMING);
            self.refocus(window, cx);
        }
    }

    /// F1 over the active tab's dialog: its page in a Help tab, when the
    /// dialog is a plain form ([`dialog_takes_help`]) and a manual is
    /// installed. `false` leaves the key to the dialog, which swallows it.
    /// The dialog stays open on its tab.
    pub(crate) fn dialog_f1(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(ask) = self.open_dialog_ask() else {
            return false;
        };
        self.open_help(ask, window, cx)
    }

    /// What F1 over the active tab's top dialog asks for, when it may.
    fn open_dialog_ask(&self) -> Option<HelpAsk> {
        if self.app_dialogs.is_open() {
            return None;
        }
        let tab = self.tabs.get(self.active)?;
        let d = tab.dialogs.top()?;
        let quitting = self.quitting || crate::close::quit_prompt_live(&self.tabs);
        dialog_takes_help(d, quitting).then(|| {
            dialog_ask(
                Some(tab.kind),
                d.id.as_str(),
                crate::ribbon_tab_name(self.ribbon_tab),
            )
        })
    }

    /// A dialog's own Help button (Task Information's, a plugin dialog's
    /// without a `help` URL): its page in a Help tab. `false` with no manual
    /// installed, which leaves the press to say the documentation is coming.
    pub(crate) fn dialog_help_button(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.tabs.get(self.active) else {
            return false;
        };
        let Some(d) = tab.dialogs.top() else {
            return false;
        };
        let ask = dialog_ask(
            Some(tab.kind),
            d.id.as_str(),
            crate::ribbon_tab_name(self.ribbon_tab),
        );
        self.open_help(ask, window, cx)
    }

    /// Open `url` in the browser and record it for `last-url`. A harness run
    /// records it only: a test never starts a browser.
    pub(crate) fn open_help_url(&mut self, url: String, cx: &mut Context<Self>) {
        if !self.harness {
            cx.open_url(&url);
        }
        self.last_opened_url = Some(url);
    }
}

#[cfg(test)]
mod tests;
