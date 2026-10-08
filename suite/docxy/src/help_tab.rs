//! The Help tab every ribbon ends with (#1021), as Word, Excel and Project
//! end theirs: a Help group (Help, Contact Support, Feedback, Show Training,
//! What's New) and our About group.
//!
//! Help and F1 say the in-app documentation is coming (it is #1022). Contact
//! Support and Feedback open the docxy GitHub new-issue page with the build
//! pre-filled ([`buildinfo::BuildInfo::feedback_url`]); under the harness the
//! URL is recorded, not opened, and `last-url` reads it. About docxy suite opens
//! the File › Account About dialog (#1023). Show Training and What's New have
//! nothing to show yet: they are drawn disabled.
use super::*;

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

/// What Help and F1 say until the documentation lands.
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
    let m = &k.modifiers;
    k.key == "f1" && !(m.control || m.platform || m.alt || m.shift || m.function)
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
            HelpAct::Help => self.set_status(DOCS_COMING),
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

    /// Open `url` in the browser and record it for `last-url`. A harness run
    /// records it only: a test never starts a browser.
    fn open_help_url(&mut self, url: String, cx: &mut Context<Self>) {
        if self.harness.is_none() {
            cx.open_url(&url);
        }
        self.last_opened_url = Some(url);
    }
}

#[cfg(test)]
mod tests;
