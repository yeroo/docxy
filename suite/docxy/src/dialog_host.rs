//! The app side of [`crate::dialog`]: a tab's dialogs take keys and button
//! presses, an accept button applies the dialog to its owner, and the window
//! draws the top dialog over everything. The harness verbs and the drawn
//! buttons call the same [`dialog_click`].
use super::*;
use crate::dialog::{Dialog, DialogOwner, DialogStack, NONE_OPEN};

/// Apply an accepted dialog to what it belongs to. An error refuses the
/// press and leaves the dialog open.
fn apply_dialog(surface: &mut Surface, dialog: &Dialog) -> Result<(), String> {
    match dialog.owner {
        DialogOwner::DeleteSummary { uid } => {
            let Surface::Project(v) = surface else {
                return Err("this dialog belongs to a Project".into());
            };
            v.ed.delete_task(uid)?;
            Ok(())
        }
        #[cfg(test)]
        DialogOwner::Test => Ok(()),
    }
}

/// Press a button on the tab's top dialog, by its label.
pub(crate) fn dialog_click(tab: &mut DocTab, button: &str) -> Result<(), String> {
    // A levelling pass asked for first runs first, as before any edit.
    flush_level_pass(tab);
    let DocTab {
        dialogs, surface, ..
    } = tab;
    dialogs.click(button, |d| apply_dialog(surface, d))?;
    complete_project(tab, true);
    Ok(())
}

/// A key while the tab has a dialog open: Enter presses the default button,
/// Escape the cancel one, and every other key (chords, Alt, Tab, typed text)
/// is swallowed, so nothing under the dialog sees it. `false` when no dialog
/// is open and the key should go on as usual.
pub(crate) fn dialog_key(tab: &mut DocTab, key: &str, m: Modifiers) -> bool {
    if !tab.dialogs.is_open() {
        return false;
    }
    let plain = !m.control && !m.alt && !m.platform;
    if let Some(label) = tab.dialogs.key_button(key, plain) {
        if let Err(e) = dialog_click(tab, &label) {
            tab.status = e.into();
        }
    }
    true
}

impl Docxy {
    /// The active tab's dialogs, when one is open.
    pub(crate) fn active_dialogs(&self) -> Option<&DialogStack> {
        self.tabs
            .get(self.active)
            .map(|t| &t.dialogs)
            .filter(|d| d.is_open())
    }

    /// Refuse a verb a person could not reach while the active tab has a
    /// dialog open: its backdrop covers the whole window, title bar and
    /// ribbon included. Shared by the harness and the Project control server.
    pub(crate) fn refuse_under_dialog(&self) -> Result<(), String> {
        match self.active_dialogs().and_then(|d| d.top()) {
            Some(d) => Err(format!("a dialog is open: {}", d.title)),
            None => Ok(()),
        }
    }

    /// Press a button on the active tab's top dialog.
    pub(crate) fn dialog_press(&mut self, button: &str) -> Result<(), String> {
        let tab = self.tabs.get_mut(self.active).ok_or(NONE_OPEN)?;
        dialog_click(tab, button)
    }

    /// A key for the active tab's dialog; see [`dialog_key`].
    pub(crate) fn dialog_takes_key(
        &mut self,
        key: &str,
        m: Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        let taken = dialog_key(tab, key, m);
        if taken {
            cx.notify();
        }
        taken
    }

    fn dialog_button_click(&mut self, button: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.dialog_press(button) {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.status = e.into();
            }
        }
        self.refocus(window, cx);
    }

    /// The top dialog, centred over a backdrop that covers the whole window,
    /// so the pointer cannot reach anything under it.
    pub(crate) fn dialog_overlay(&self, pal: Pal, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.active_dialogs()?.top()?;
        let tabs = (!d.tabs.is_empty()).then(|| {
            h_flex()
                .gap_3()
                .children(d.tabs.iter().enumerate().map(|(i, t)| {
                    div()
                        .text_size(px(12.))
                        .text_color(if i == d.tab { pal.fg } else { pal.dim })
                        .when(i == d.tab, |t| t.border_b_1().border_color(hsla_u(BRAND)))
                        .child(SharedString::from(t.replace('&', "")))
                }))
        });
        // Form controls read-only for now; the first form dialog draws
        // editable widgets over `Dialog::set`.
        let controls = d.page_controls().filter(|c| c.visible).map(|c| {
            h_flex()
                .gap_2()
                .text_size(px(12.))
                .text_color(if c.enabled { pal.fg } else { pal.dim })
                .child(SharedString::from(c.label.replace('&', "")))
                .child(SharedString::from(c.text()))
        });
        let buttons = d.buttons.iter().enumerate().map(|(i, b)| {
            let label = b.label.clone();
            div()
                .id(("dialog-button", i))
                .relative()
                .min_w(px(72.))
                .px_3()
                .py_1()
                .flex()
                .justify_center()
                .text_size(px(12.))
                .border_1()
                .border_color(if b.default { hsla_u(BRAND) } else { pal.border })
                .text_color(if b.enabled { pal.fg } else { pal.dim })
                .child(probe(&self.probes, format!("dialog-button:{}", b.label)))
                .child(SharedString::from(b.label.replace('&', "")))
                .when(b.enabled, |el| {
                    el.cursor_pointer()
                        .hover(|el| el.bg(pal.hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.dialog_button_click(&label, window, cx)
                        }))
                })
        });
        Some(
            deferred(
                div()
                    .id("dialog-backdrop")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(Hsla {
                        a: 0.25,
                        ..hsla_u(0x000000)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        v_flex()
                            .id("dialog")
                            .relative()
                            .min_w(px(320.))
                            .max_w(px(560.))
                            .p_4()
                            .gap_3()
                            .bg(pal.panel)
                            .border_1()
                            .border_color(pal.border)
                            .shadow_lg()
                            .child(probe(&self.probes, "dialog"))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(pal.fg)
                                    .child(SharedString::from(d.title.clone())),
                            )
                            .when_some(tabs, |el, t| el.child(t))
                            .when_some(d.text.clone(), |el, text| {
                                el.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(pal.fg)
                                        .child(SharedString::from(text)),
                                )
                            })
                            .children(controls)
                            .child(h_flex().gap_2().justify_end().children(buttons)),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}
