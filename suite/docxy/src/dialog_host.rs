//! The app side of [`crate::dialog`]: a tab's dialogs take keys and button
//! presses, an accept button applies the dialog to its owner, and the window
//! draws the top dialog over everything. The harness verbs and the drawn
//! buttons call the same [`dialog_click`].
use super::*;
use crate::dialog::{self, ControlKind, Dialog, DialogOwner, DialogStack, NONE_OPEN};

/// Apply an accepted dialog to what it belongs to. An error refuses the
/// press and leaves the dialog open. `Ok(true)` when it changed a document
/// (a Project tracks its own changes); an untouched OK changes nothing.
fn apply_dialog(
    surface: &mut Surface,
    pkg: Option<&mut Package>,
    hf: Option<&mut Editor>,
    dialog: &Dialog,
) -> Result<bool, String> {
    match dialog.owner {
        DialogOwner::InsertTable
        | DialogOwner::DeleteCells
        | DialogOwner::SplitCells
        | DialogOwner::SortTable
        | DialogOwner::TableToText
        | DialogOwner::TextToTable => {
            // A table dialog acts where the caret is typing: the open header
            // or footer, else the body.
            let ed = match (hf, surface) {
                (Some(ed), _) => ed,
                (None, Surface::Doc(ed)) => ed,
                _ => return Err("this dialog belongs to a document".into()),
            };
            crate::table_dialogs::apply_table_dialog(ed, dialog)
        }
        DialogOwner::DeleteSummary { uid } => {
            let Surface::Project(v) = surface else {
                return Err("this dialog belongs to a Project".into());
            };
            v.ed.delete_task(uid)?;
            Ok(false)
        }
        DialogOwner::PageSetup | DialogOwner::Columns => {
            let (Surface::Doc(ed), Some(pkg)) = (surface, pkg) else {
                return Err("this dialog belongs to a .docx document".into());
            };
            match dialog.owner {
                DialogOwner::PageSetup => crate::page_setup::apply_page_setup(ed, pkg, dialog),
                _ => crate::page_setup::apply_columns(ed, pkg, dialog),
            }
        }
        DialogOwner::HfDistance { is_header, section } => {
            let Surface::Doc(ed) = surface else {
                return Err("this dialog belongs to a document".into());
            };
            crate::hf_tab::apply_distance(ed, dialog, is_header, section)
        }
        DialogOwner::PageNumberFormat { section } => {
            let Surface::Doc(ed) = surface else {
                return Err("this dialog belongs to a document".into());
            };
            crate::page_number::apply_format(ed, dialog, section)
        }
        // Their presses are handled in `ttc_dialog::click`, before this.
        DialogOwner::TextToColumns { .. } | DialogOwner::TextToColumnsReplace => {
            Err("Text to Columns applies through its own wizard".into())
        }
        // Handled in `sheet_consolidate::click`, before this.
        DialogOwner::Consolidate { .. } => Err("Consolidate applies through the Data tab".into()),
        // Handled in `sheet_filter::click` and `sheet_sort::click`, before this.
        DialogOwner::FilterMenu { .. }
        | DialogOwner::CustomFilter { .. }
        | DialogOwner::Top10Filter { .. }
        | DialogOwner::AdvancedFilter { .. } => Err("a filter applies through the Data tab".into()),
        DialogOwner::SortLevels { .. } | DialogOwner::SortWarning { .. } => {
            Err("a sort applies through the Data tab".into())
        }
        // Handled in `sheet_outline::click`, before this.
        DialogOwner::Subtotal { .. }
        | DialogOwner::OutlineSettings
        | DialogOwner::OutlineAxis { .. } => {
            Err("an outline dialog applies through the Data tab".into())
        }
        // Handled in `reopen_click`, before this: Yes replaces the whole tab.
        DialogOwner::Reopen { .. } => Err("reopening replaces the tab".into()),
        // Handled in `mailings_dialogs::click`, before this: they act on the
        // whole tab and may open a document.
        DialogOwner::MailEnvelopes
        | DialogOwner::MailEnvelopeOptions
        | DialogOwner::MailEnvelopesReplace(_)
        | DialogOwner::MailLabels
        | DialogOwner::MailLabelOptions
        | DialogOwner::MailLabelsReplace(_)
        | DialogOwner::MailRecipients
        | DialogOwner::MailAddressBlock
        | DialogOwner::MailGreetingLine
        | DialogOwner::MailMatchFields
        | DialogOwner::MailFind
        | DialogOwner::MailCheckErrors
        | DialogOwner::MailMergeToNew
        | DialogOwner::MailAttach { .. }
        | DialogOwner::MailReport => Err("a mail merge dialog applies through Mailings".into()),
        // Handled in `design_dialogs::click`, before this; Options... writes
        // back into Page Borders in the dialog stack.
        DialogOwner::DesignMoreColors
        | DialogOwner::DesignFillEffects
        | DialogOwner::DesignWatermark
        | DialogOwner::DesignPageBorders
        | DialogOwner::DesignBorderOptions => {
            Err("a Design dialog applies through the Design tab".into())
        }
        // Handled in `close::close_prompt_click`, before this: it closes
        // the tab, or goes on with the window's close.
        DialogOwner::SaveOnClose { .. } => Err("closing a tab applies through the app".into()),
        // Handled in `user_name::click`, before this: it is the app's.
        DialogOwner::UserName => Err("the user name is an app setting".into()),
        // Handled in `Docxy::about_click`, before this: it only copies or closes.
        DialogOwner::About => Err("the About dialog only copies or closes".into()),
        #[cfg(test)]
        DialogOwner::Test | DialogOwner::TestChild => Ok(false),
    }
}

/// Yes on the reopen question (#610): the tab's file, loaded again in the
/// mode that was asked for, replaces the tab and its unsaved changes. `None`
/// for any other dialog or button (No is the cancel button and just closes).
fn reopen_click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let DialogOwner::Reopen { mode } = top.owner else {
        return None;
    };
    let accept = top
        .buttons
        .iter()
        .find(|b| b.label.eq_ignore_ascii_case(button))
        .is_some_and(|b| b.role == dialog::ButtonRole::Accept);
    if !accept {
        return None;
    }
    let Some(path) = tab.path.clone() else {
        return Some(Err("this tab has no file to reopen".into()));
    };
    let trusted = crate::trusted::TrustStore::load(&crate::config_root());
    Some(crate::tab_from_path_mode(&path, mode, &trusted).map(|fresh| *tab = fresh))
}

/// Whether the active tab's top dialog is the reopen question, whose Yes
/// replaces the tab under any grid state the window keeps for it.
fn reopen_on_top(tab: Option<&DocTab>) -> bool {
    tab.and_then(|t| t.dialogs.top())
        .is_some_and(|d| matches!(d.owner, DialogOwner::Reopen { .. }))
}

/// Press a button on the tab's top dialog, by its label.
pub(crate) fn dialog_click(tab: &mut DocTab, button: &str) -> Result<(), String> {
    // A levelling pass asked for first runs first, as before any edit.
    flush_level_pass(tab);
    // Text to Columns asks its own question before it applies.
    if let Some(done) = crate::ttc_dialog::click(tab, button) {
        return done;
    }
    // So do the outline dialogs (#693): Subtotal's OK and Remove All.
    if let Some(done) = crate::sheet_outline::click(tab, button) {
        return done;
    }
    // Consolidate's Add and Delete edit its list; OK consolidates (#694).
    if let Some(done) = crate::sheet_consolidate::click(tab, button) {
        return done;
    }
    // The filter drop-down and its dialogs (#690), and the sorts (#691).
    if let Some(done) = crate::sheet_filter::click(tab, button) {
        return done;
    }
    if let Some(done) = crate::sheet_sort::click(tab, button) {
        return done;
    }
    if let Some(done) = reopen_click(tab, button) {
        return done;
    }
    if let Some(done) = crate::mailings_dialogs::click(tab, button) {
        return done;
    }
    if let Some(done) = crate::design_dialogs::click(tab, button) {
        return done;
    }
    let DocTab {
        dialogs,
        surface,
        pkg,
        hf_edit,
        ..
    } = tab;
    let mut changed = false;
    dialogs.click(button, |d| {
        changed = apply_dialog(
            surface,
            pkg.as_mut(),
            hf_edit.as_mut().map(|h| &mut h.editor),
            d,
        )?;
        Ok(())
    })?;
    if changed {
        tab.set_dirty();
    }
    complete_project(tab, true);
    Ok(())
}

/// A key while the tab has a dialog open: Enter presses the default button,
/// Escape the cancel one, Tab and Shift+Tab move the focus, and the focused
/// widget takes the rest: typed characters and Backspace edit a field, Space
/// toggles a checkbox, Up and Down step a radio group or dropdown. Every key
/// (chords and Alt too) is swallowed, so nothing under the dialog sees it.
/// `typed` is the character the key types, when it types one. `false` when
/// no dialog is open and the key should go on as usual.
pub(crate) fn dialog_key(tab: &mut DocTab, key: &str, typed: Option<&str>, m: Modifiers) -> bool {
    if !tab.dialogs.is_open() {
        return false;
    }
    let plain = !m.control && !m.alt && !m.platform;
    if let Some(label) = tab.dialogs.key_button(key, plain) {
        if let Err(e) = dialog_click(tab, &label) {
            tab.status = e.into();
        }
        return true;
    }
    if !plain {
        return true;
    }
    let Ok(d) = tab.dialogs.top_dialog_mut() else {
        return true;
    };
    let typed = typed.map(str::to_string).or_else(|| {
        // A synthetic key without its character: a one-letter key name.
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if m.shift => Some(c.to_uppercase().collect()),
            (Some(c), None) => Some(c.to_string()),
            _ => None,
        }
    });
    let done = match key {
        "tab" => {
            d.focus_step(m.shift);
            Ok(())
        }
        "backspace" => d.backspace(),
        "space" => d.space(),
        "up" => d.step_focused(false),
        "down" => d.step_focused(true),
        _ => match typed
            .as_deref()
            .map(|t| (t.chars().next(), t.chars().count()))
        {
            Some((Some(c), 1)) if !c.is_control() => d.type_char(c),
            _ => Ok(()),
        },
    };
    if let Err(e) = done {
        tab.status = e.into();
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
    pub(crate) fn dialog_press(
        &mut self,
        button: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // The user name is the app's setting, not the tab's (#620).
        if let Some(done) = self.user_name_click(button) {
            return done;
        }
        // About's Copy and Close are the app's too (#1023).
        if let Some(done) = self.about_click(button, cx) {
            return done;
        }
        // The close prompt closes the tab, or goes on with the window's
        // close (#629, #630).
        if let Some(done) = self.close_prompt_click(button, window, cx) {
            return done;
        }
        let reopen = reopen_on_top(self.tabs.get(self.active));
        let tab = self.tabs.get_mut(self.active).ok_or(NONE_OPEN)?;
        dialog_click(tab, button)?;
        if reopen {
            self.after_reopen();
        }
        // A merge or a sheet of labels opens as a new document.
        self.take_mail_outputs();
        Ok(())
    }

    /// The tab may have been replaced by its file: nothing the window held
    /// for the old one applies, and the session should say what is open.
    fn after_reopen(&mut self) {
        self.drop_grid_state();
        self.persist();
    }

    /// A key for the active tab's dialog; see [`dialog_key`].
    pub(crate) fn dialog_takes_key(
        &mut self,
        key: &str,
        typed: Option<&str>,
        m: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Enter or Escape on a dialog the app owns (the user name, About, the close
        // prompt) presses through the app, as its drawn buttons do.
        let plain = !m.control && !m.alt && !m.platform;
        let app_button = self
            .tabs
            .get(self.active)
            .filter(|t| {
                t.dialogs.top().is_some_and(|d| {
                    matches!(
                        d.owner,
                        DialogOwner::UserName
                            | DialogOwner::About
                            | DialogOwner::SaveOnClose { .. }
                    )
                })
            })
            .and_then(|t| t.dialogs.key_button(key, plain));
        if let Some(label) = app_button {
            if let Err(e) = self.dialog_press(&label, window, cx) {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.status = e.into();
                }
            }
            cx.notify();
            return true;
        }
        let reopen = reopen_on_top(self.tabs.get(self.active));
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        let taken = dialog_key(tab, key, typed, m);
        if taken {
            if reopen {
                self.after_reopen();
            }
            self.take_mail_outputs();
            cx.notify();
        }
        taken
    }

    fn dialog_button_click(&mut self, button: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.dialog_press(button, window, cx) {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.status = e.into();
            }
        }
        self.refocus(window, cx);
    }

    /// A press on one of the top dialog's widgets: see [`Dialog::click_control`].
    fn dialog_control_click(&mut self, index: usize, item: Option<usize>, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let done = tab
                .dialogs
                .top_dialog_mut()
                .and_then(|d| d.click_control(index, item));
            if let Err(e) = done {
                tab.status = e.into();
            }
        }
        cx.notify();
    }

    /// A press on the top dialog's tab strip.
    fn dialog_tab_click(&mut self, label: &str, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Err(e) = tab.dialogs.select_tab(label) {
                tab.status = e.into();
            }
        }
        cx.notify();
    }

    /// One control's widget: a field, checkbox, radio group or dropdown that
    /// takes the pointer, or a list, grid or label drawn as text.
    fn dialog_widget(&self, d: &Dialog, i: usize, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let c = &d.controls[i];
        let focused = d.focus == Some(i) && d.focused().is_some();
        let fg = if c.enabled { pal.fg } else { pal.dim };
        let label = SharedString::from(c.label.replace('&', ""));
        let editable = c.enabled && c.kind.is_editable();
        let row = h_flex()
            .id(("dialog-control", i))
            .gap_2()
            .items_center()
            .text_size(px(12.))
            .text_color(fg)
            .child(probe(&self.probes, format!("dialog-control:{}", c.name)));
        let boxed = |text: String| {
            div()
                .min_w(px(96.))
                .px_1()
                .border_1()
                .border_color(if focused { hsla_u(BRAND) } else { pal.dim })
                .bg(pal.panel)
                .child(SharedString::from(text))
        };
        match c.kind {
            k if k.is_text() => {
                let caret = if focused { "|" } else { "" };
                row.child(label)
                    .child(boxed(format!("{}{caret}", c.text())))
                    .when(editable, |r| {
                        r.cursor_text().on_click(on_widget(cx, i, None))
                    })
                    .into_any_element()
            }
            ControlKind::Checkbox => {
                let on = matches!(c.value, dialog::Value::Bool(true));
                row.child(boxed(if on { "\u{2713}" } else { " " }.into()).min_w(px(16.)))
                    .child(label)
                    .when(editable, |r| {
                        r.cursor_pointer().on_click(on_widget(cx, i, None))
                    })
                    .into_any_element()
            }
            ControlKind::Radio => {
                let chosen = match c.value {
                    dialog::Value::Choice(i) => i,
                    _ => None,
                };
                let items = c.items.iter().enumerate().map(|(k, item)| {
                    let mark = if chosen == Some(k) {
                        "\u{25C9}"
                    } else {
                        "\u{25CB}"
                    };
                    div()
                        .id(("dialog-radio", i * 64 + k))
                        .flex()
                        .gap_1()
                        .child(format!("{mark} {item}"))
                        .when(editable, |el| {
                            el.cursor_pointer().on_click(on_widget(cx, i, Some(k)))
                        })
                });
                row.child(label)
                    .when(focused, |r| r.border_b_1().border_color(hsla_u(BRAND)))
                    .children(items)
                    .into_any_element()
            }
            ControlKind::Dropdown => row
                .child(label)
                .child(boxed(format!("{} \u{25BE}", c.text())))
                .when(editable, |r| {
                    r.cursor_pointer().on_click(on_widget(cx, i, None))
                })
                .into_any_element(),
            ControlKind::CheckList => {
                // Virtualised: an AutoFilter list holds up to 10,000 values.
                let n = c.items.len();
                let list = uniform_list(
                    ("dialog-checklist", i),
                    n,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        let Some(c) = this
                            .active_dialogs()
                            .and_then(|s| s.top())
                            .and_then(|d| d.controls.get(i))
                        else {
                            return vec![];
                        };
                        let checks = match &c.value {
                            dialog::Value::Checks(v) => v.clone(),
                            _ => Vec::new(),
                        };
                        range
                            .filter(|&k| k < c.items.len())
                            .map(|k| {
                                let on = checks.get(k).copied().unwrap_or(false);
                                let depth = c.depths.get(k).copied().unwrap_or(0);
                                h_flex()
                                    .id(("dialog-check", k))
                                    .h(px(18.))
                                    .pl(px(4. + 12. * f32::from(depth)))
                                    .gap_1()
                                    .cursor_pointer()
                                    .child(if on { "\u{2611}" } else { "\u{2610}" })
                                    .child(SharedString::from(c.items[k].clone()))
                                    .on_click(on_widget(cx, i, Some(k)))
                                    .into_any_element()
                            })
                            .collect()
                    }),
                )
                .h(px(220.))
                .w_full();
                v_flex()
                    .id(("dialog-control", i))
                    .gap_1()
                    .text_size(px(12.))
                    .text_color(fg)
                    .child(probe(&self.probes, format!("dialog-control:{}", c.name)))
                    .child(label)
                    .child(
                        div()
                            .border_1()
                            .border_color(if focused { hsla_u(BRAND) } else { pal.dim })
                            .bg(pal.panel)
                            .child(list),
                    )
                    .into_any_element()
            }
            _ => row
                .child(label)
                .child(SharedString::from(c.text()))
                .into_any_element(),
        }
    }

    /// The top dialog, centred over a backdrop that covers the whole window,
    /// so the pointer cannot reach anything under it.
    pub(crate) fn dialog_overlay(&self, pal: Pal, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.active_dialogs()?.top()?;
        let tabs = (!d.tabs.is_empty()).then(|| {
            h_flex()
                .gap_3()
                .children(d.tabs.iter().enumerate().map(|(i, t)| {
                    let label = t.clone();
                    div()
                        .id(("dialog-tab", i))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .text_color(if i == d.tab { pal.fg } else { pal.dim })
                        .when(i == d.tab, |t| t.border_b_1().border_color(hsla_u(BRAND)))
                        .child(probe(&self.probes, format!("dialog-tab:{t}")))
                        .child(SharedString::from(t.replace('&', "")))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.dialog_tab_click(&label, cx)),
                        )
                }))
        });
        let controls: Vec<AnyElement> = d
            .shown_indices()
            .into_iter()
            .map(|i| self.dialog_widget(d, i, pal, cx))
            .collect();
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

/// The click handler of a dialog widget: control `i`, and the radio item.
fn on_widget(
    cx: &mut Context<Docxy>,
    i: usize,
    item: Option<usize>,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |this, _: &ClickEvent, _, cx| this.dialog_control_click(i, item, cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::{Button, ButtonRole, Control, Value};
    use core::prelude::v1::test;

    /// A document tab with a two-field form open: Top (a number) and Note.
    fn tab_with_form() -> DocTab {
        let mut t = tab_from_path(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
        );
        let mut d = Dialog::message("form", "Form", String::new(), &[], DialogOwner::Test);
        d.text = None;
        d.controls = vec![
            Control::new("top", "Top:", ControlKind::Number, Value::Text("1".into())),
            Control::new(
                "note",
                "Note:",
                ControlKind::Text,
                Value::Text(String::new()),
            ),
            Control::new("on", "On", ControlKind::Checkbox, Value::Bool(false)),
        ];
        d.buttons = vec![
            Button {
                default: true,
                ..Button::new("OK", ButtonRole::Accept)
            },
            Button::new("Cancel", ButtonRole::Cancel),
        ];
        t.dialogs.push(d);
        t
    }

    fn value(t: &DocTab, name: &str) -> Value {
        t.dialogs.top().unwrap().value(name).unwrap().clone()
    }

    fn key(t: &mut DocTab, key: &str, typed: Option<&str>) -> bool {
        dialog_key(t, key, typed, Modifiers::default())
    }

    /// Typed characters and Backspace reach the focused field, Tab and
    /// Shift+Tab move the focus, Space toggles a checkbox, and Enter still
    /// presses OK (#649). Before, the dialog swallowed every one of them.
    #[test]
    fn keys_edit_the_focused_widget_and_enter_still_presses_ok() {
        let mut t = tab_with_form();
        assert!(key(&mut t, "tab", None));
        assert!(key(&mut t, "backspace", None));
        assert!(key(&mut t, "2", Some("2")));
        assert!(key(&mut t, ".", Some(".")));
        // A synthetic key with no character types its one-letter name.
        assert!(key(&mut t, "5", None));
        assert_eq!(value(&t, "top"), Value::Text("2.5".into()));
        assert!(key(&mut t, "x", Some("x")));
        assert_eq!(value(&t, "top"), Value::Text("2.5".into()), "refused");
        assert_eq!(t.status.as_ref(), "'Top:' takes a number");
        assert!(key(&mut t, "tab", None));
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert!(dialog_key(&mut t, "h", None, shift));
        assert!(key(&mut t, "i", Some("i")));
        assert!(key(&mut t, "space", Some(" ")));
        assert_eq!(value(&t, "note"), Value::Text("Hi ".into()));
        assert!(dialog_key(&mut t, "tab", None, shift));
        assert!(key(&mut t, "3", Some("3")));
        assert_eq!(value(&t, "top"), Value::Text("2.53".into()));
        assert!(key(&mut t, "tab", None));
        assert!(key(&mut t, "tab", None));
        assert!(key(&mut t, "space", Some(" ")));
        assert_eq!(value(&t, "on"), Value::Bool(true));
        // A chord or Alt is swallowed and edits nothing, even in a focused
        // text field that a plain "a" would type into.
        assert!(dialog_key(&mut t, "tab", None, shift));
        assert_eq!(
            t.dialogs.top().unwrap().focused().map(|c| c.name),
            Some("note")
        );
        for m in [
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ] {
            assert!(dialog_key(&mut t, "a", Some("a"), m));
            assert!(dialog_key(&mut t, "backspace", None, m));
        }
        assert_eq!(value(&t, "note"), Value::Text("Hi ".into()));
        assert_eq!(value(&t, "on"), Value::Bool(true));
        assert!(key(&mut t, "enter", None));
        assert!(!t.dialogs.is_open(), "Enter pressed OK");
        assert!(!key(&mut t, "a", Some("a")), "no dialog, the key goes on");
    }
}
