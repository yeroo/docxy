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
        // Handled in `sheet_goto::click`, before this.
        DialogOwner::GoTo | DialogOwner::GoToSpecial => {
            Err("Go To applies through Find & Select".into())
        }
        // Handled in `Docxy::paste_dialog_click`, before this.
        DialogOwner::PasteSpecial { .. } => Err("Paste Special applies through Paste".into()),
        // Handled in `Docxy::drop_dialog_click`, before this.
        DialogOwner::DropReplace => Err("the drop applies through the grid".into()),
        // Handled in `Docxy::fill_dialog_click`, before this.
        DialogOwner::Series | DialogOwner::JustifyOverflow | DialogOwner::CustomLists => {
            Err("a Fill dialog applies through Home › Fill".into())
        }
        // Handled in `sheet_consolidate::click`, before this.
        DialogOwner::Consolidate { .. } => Err("Consolidate applies through the Data tab".into()),
        // Handled in `sheet_validation::click` and `alert_click`, before this.
        DialogOwner::DataValidation { .. } | DialogOwner::DataValidationAlert => {
            Err("data validation applies through the Data tab".into())
        }
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
        // Handled in `sheet_page_setup::click`, before this.
        DialogOwner::SheetPageSetup => Err("page setup applies through the Page Layout tab".into()),
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
        DialogOwner::Message => Ok(false),
        // Handled in `sheet_autocorrect::click`, before this: the app's.
        DialogOwner::AutoCorrect
        | DialogOwner::AutoCorrectExceptions
        | DialogOwner::AutoCorrectRedefine => Err("AutoCorrect is an app setting".into()),
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

/// Whether Enter and Escape on a dialog of `owner` press through
/// `Docxy::dialog_press`, which the app's own dialogs need (the user name,
/// About, AutoCorrect, the close prompt, the fill, paste and drop dialogs),
/// as does Go To's move to another sheet (#707 r6 m2).
pub(crate) fn presses_through_app(owner: &DialogOwner) -> bool {
    crate::sheet_autocorrect::is_autocorrect(*owner)
        || matches!(
            owner,
            DialogOwner::UserName
                | DialogOwner::About
                | DialogOwner::SaveOnClose { .. }
                | DialogOwner::Series
                | DialogOwner::JustifyOverflow
                | DialogOwner::CustomLists
                | DialogOwner::PasteSpecial { .. }
                | DialogOwner::DropReplace
                | DialogOwner::GoTo
                | DialogOwner::GoToSpecial
        )
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
    // Page Layout's Page Setup (#1019).
    if let Some(done) = crate::sheet_page_setup::click(tab, button) {
        return done;
    }
    // Go To's OK selects; its Special… opens Go To Special (#671).
    if let Some(done) = crate::sheet_goto::click(tab, button) {
        return done;
    }
    // Consolidate's Add and Delete edit its list; OK consolidates (#694).
    if let Some(done) = crate::sheet_consolidate::click(tab, button) {
        return done;
    }
    // The data-validation alert and dialog (#687, #689).
    if let Some(done) = crate::sheet_validation::alert_click(tab, button) {
        return done;
    }
    if let Some(done) = crate::sheet_validation::click(tab, button) {
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

/// [`dialog_key_with`] with nothing on the clipboard, for the tests.
#[cfg(test)]
pub(crate) fn dialog_key(tab: &mut DocTab, key: &str, typed: Option<&str>, m: Modifiers) -> bool {
    dialog_key_with(tab, key, typed, m, None)
}

/// A key while the tab has a dialog open: Enter presses the default button,
/// Escape the cancel one, Tab and Shift+Tab move the focus, and the focused
/// widget takes the rest: a field takes typed characters, Backspace, Delete,
/// the arrows, Home and End (Shift extends a selection), Ctrl+A and Ctrl+V
/// (Ctrl+C, which writes the clipboard, is `Docxy::dialog_takes_key`'s);
/// Space toggles a checkbox, Up and Down step a radio group or dropdown.
/// Every key (chords and Alt too) is swallowed, so nothing under the dialog
/// sees it. `typed` is the character the key types, when it types one.
/// `false` when no dialog is open and the key should go on as usual.
/// `clip` is the text Ctrl+V pastes.
pub(crate) fn dialog_key_with(
    tab: &mut DocTab,
    key: &str,
    typed: Option<&str>,
    m: Modifiers,
    clip: Option<&str>,
) -> bool {
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
    if let Err(e) = edit_key(&mut tab.dialogs, key, typed, m, clip) {
        tab.status = e.into();
    }
    true
}

/// The part of [`dialog_key_with`] that edits the top dialog's focused widget.
pub(crate) fn edit_key(
    stack: &mut DialogStack,
    key: &str,
    typed: Option<&str>,
    m: Modifiers,
    clip: Option<&str>,
) -> Result<(), String> {
    let Ok(d) = stack.top_dialog_mut() else {
        return Ok(());
    };
    if m.alt {
        return Ok(());
    }
    if m.control || m.platform {
        // Only select-all and paste; any other chord is swallowed.
        return match (key, clip) {
            ("a", _) => {
                d.select_all();
                Ok(())
            }
            ("v", Some(text)) => {
                let line: String = text.chars().filter(|c| !c.is_control()).collect();
                d.insert_text(&line)
            }
            _ => Ok(()),
        };
    }
    let typed = typed.map(str::to_string).or_else(|| {
        // A synthetic key without its character: a one-letter key name.
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if m.shift => Some(c.to_uppercase().collect()),
            (Some(c), None) => Some(c.to_string()),
            _ => None,
        }
    });
    match key {
        "tab" => {
            d.focus_step(m.shift);
            Ok(())
        }
        "backspace" => d.backspace(),
        "delete" => d.delete(),
        "left" | "right" => {
            d.move_caret(key == "right", m.shift);
            Ok(())
        }
        "home" | "end" => {
            d.move_caret_edge(key == "end", m.shift);
            Ok(())
        }
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
    }
}

/// The dialogs a person sees: the app's own stack when it holds one (a tab
/// that arrives while it is open does not hide it), else the active tab's.
fn shown_stack<'a>(app: &'a DialogStack, tab: Option<&'a DialogStack>) -> Option<&'a DialogStack> {
    if app.is_open() {
        return Some(app);
    }
    tab.filter(|d| d.is_open())
}

impl Docxy {
    /// The dialogs a person sees: the app's own stack when one is open (the
    /// User name opened with no document, #1027; a tab that arrives later
    /// does not hide it), else the active tab's. `None` when none is open.
    pub(crate) fn active_dialogs(&self) -> Option<&DialogStack> {
        shown_stack(
            &self.app_dialogs,
            self.tabs.get(self.active).map(|t| &t.dialogs),
        )
    }

    /// The stack [`Docxy::active_dialogs`] reads, to change it: the app's
    /// when it holds one or no document is open, else the active tab's.
    pub(crate) fn active_dialogs_mut(&mut self) -> &mut DialogStack {
        match self.tabs.get_mut(self.active) {
            Some(t) if !self.app_dialogs.is_open() => &mut t.dialogs,
            _ => &mut self.app_dialogs,
        }
    }

    /// Refuse a verb a person could not reach while a dialog is open (the
    /// active tab's or the app's): its backdrop covers the whole window, title bar and
    /// ribbon included. Shared by the harness and the Project control server.
    pub(crate) fn refuse_under_dialog(&self) -> Result<(), String> {
        match self.active_dialogs().and_then(|d| d.top()) {
            Some(d) => Err(format!("a dialog is open: {}", d.title)),
            None => Ok(()),
        }
    }

    /// Press a button on the top dialog: the app's User name, else the
    /// active tab's.
    pub(crate) fn dialog_press(
        &mut self,
        button: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // The user name is the app's setting, not the tab's (#620).
        if let Some(done) = self.user_name_click(button, cx) {
            return done;
        }
        // About's Copy and Close are the app's too (#1023).
        if let Some(done) = self.about_click(button, cx) {
            return done;
        }
        // So is AutoCorrect (#667).
        if let Some(done) = self.autocorrect_click(button, cx) {
            return done;
        }
        // The close prompt closes the tab, or goes on with the window's
        // close (#629, #630).
        if let Some(done) = self.close_prompt_click(button, window, cx) {
            return done;
        }
        // So are the custom lists, which the Series dialog reads (#668).
        if let Some(done) = self.fill_dialog_click(button, cx) {
            return done;
        }
        // And the copy Paste Special pastes (#669).
        if let Some(done) = self.paste_dialog_click(button) {
            return done;
        }
        // And a drop by the selection's border waiting on its question (#670).
        if let Some(done) = self.drop_dialog_click(button) {
            return done;
        }
        let reopen = reopen_on_top(self.tabs.get(self.active));
        let sheet_before = self.active_sheet().map(|v| v.active);
        let tab = self.tabs.get_mut(self.active).ok_or(NONE_OPEN)?;
        dialog_click(tab, button)?;
        if reopen {
            self.after_reopen(cx);
        }
        // A dialog that moved to another sheet (Go To) leaves the grid state
        // of the one it left behind, as a sheet-tab click does (#707 r5 M3).
        if !reopen && self.active_sheet().map(|v| v.active) != sheet_before {
            self.drop_grid_state();
        }
        // A merge or a sheet of labels opens as a new document.
        self.take_mail_outputs(cx);
        Ok(())
    }

    /// The tab may have been replaced by its file: nothing the window held
    /// for the old one applies, and the session should say what is open.
    fn after_reopen(&mut self, cx: &mut App) {
        self.drop_grid_state();
        self.persist(cx);
    }

    /// A key for the open dialog, the app's or the active tab's; see
    /// [`dialog_key_with`].
    pub(crate) fn dialog_takes_key(
        &mut self,
        key: &str,
        typed: Option<&str>,
        m: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Enter or Escape on a dialog the app applies (the user name, About,
        // AutoCorrect, the close prompt, the fill, paste and drop dialogs)
        // presses through the app, as its drawn buttons do; so do Go To and
        // Go To Special, whose OK may move to another sheet and must drop the
        // grid state left behind, as a click does (#707 r6 m2).
        let plain = !m.control && !m.alt && !m.platform;
        let app_button = self
            .active_dialogs()
            .filter(|s| s.top().is_some_and(|d| presses_through_app(&d.owner)))
            .and_then(|s| s.key_button(key, plain));
        if let Some(label) = app_button {
            if let Err(e) = self.dialog_press(&label, window, cx) {
                self.set_status(e);
            }
            cx.notify();
            return true;
        }
        // Ctrl+C copies the focused field's selection (#1029); with none it
        // copies nothing, and like every chord it goes no further.
        let chord = (m.control || m.platform) && !m.alt && !m.shift;
        if chord && key == "c" {
            let Some(stack) = self.active_dialogs() else {
                return false;
            };
            if let Some(text) = stack.top().and_then(Dialog::selected_text) {
                self.clipboard_write(text, cx);
            }
            return true;
        }
        // Ctrl+V pastes what the clipboard holds.
        let clip = ((m.control || m.platform) && key == "v" && self.active_dialogs().is_some())
            .then(|| match self.clipboard_read(cx) {
                ClipRead::Text(t) => Some(t),
                _ => None,
            })
            .flatten();
        if self.app_dialogs.is_open() {
            // The app's own dialog (the User name opened with no document). It
            // stays on top if a tab arrives under it. A refusal goes to the
            // active tab's status line, if there is a tab.
            if let Err(e) = edit_key(&mut self.app_dialogs, key, typed, m, clip.as_deref()) {
                self.set_status(e);
            }
            cx.notify();
            return true;
        }
        let reopen = reopen_on_top(self.tabs.get(self.active));
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        let taken = dialog_key_with(tab, key, typed, m, clip.as_deref());
        if taken {
            if reopen {
                self.after_reopen(cx);
            }
            self.take_mail_outputs(cx);
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
        let done = self
            .active_dialogs_mut()
            .top_dialog_mut()
            .and_then(|d| d.click_control(index, item));
        if let Err(e) = done {
            self.set_status(e);
        }
        cx.notify();
    }

    /// A click on a text field: focus it and put the caret where the click
    /// fell, in the characters its text is drawn in.
    fn dialog_field_click(
        &mut self,
        index: usize,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dialog_control_click(index, None, cx);
        let Some(name) = self
            .active_dialogs()
            .and_then(|s| s.top())
            .and_then(|d| d.controls.get(index))
            .map(|c| c.name)
        else {
            return;
        };
        let Some(bounds) = self
            .probes
            .borrow()
            .current(&format!("dialog-field:{name}"))
        else {
            return;
        };
        // The probe is already inside the border: the text starts a padding in.
        let x = f32::from(at.x - bounds.left()) - FIELD_INSET;
        let measurer = Measurer::new(window);
        if let Some(d) = self
            .active_dialogs_mut()
            .top_dialog_mut()
            .ok()
            .filter(|d| d.focus == Some(index))
        {
            let text = d.focused().map(|c| c.text()).unwrap_or_default();
            let to = char_at_x(&text, x, |s| measurer.width(s, 12., false, false));
            d.move_caret_to(to, false);
        }
        cx.notify();
    }

    /// A press on the top dialog's tab strip.
    fn dialog_tab_click(&mut self, label: &str, cx: &mut Context<Self>) {
        if let Err(e) = self.active_dialogs_mut().select_tab(label) {
            self.set_status(e);
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
        let boxed_with = |content: AnyElement| {
            div()
                .min_w(px(96.))
                .px_1()
                .border_1()
                .border_color(if focused { hsla_u(BRAND) } else { pal.dim })
                .bg(pal.panel)
                .child(content)
        };
        let boxed = |text: String| boxed_with(SharedString::from(text).into_any_element());
        match c.kind {
            k if k.is_text() => {
                let name = c.name;
                // The text, with the caret in it and the selection shaded.
                let text = c.text();
                let shown: AnyElement = if focused {
                    let chars: Vec<char> = text.chars().collect();
                    let caret = d.caret_at().min(chars.len());
                    let (from, to) = d.selection().unwrap_or((caret, caret));
                    let part = |a: usize, b: usize| {
                        SharedString::from(chars[a..b].iter().collect::<String>())
                    };
                    let bar = || caret_bar(pal.fg);
                    h_flex()
                        .child(part(0, from))
                        .when(caret == from, |r| r.child(bar()))
                        .when(from != to, |r| {
                            r.child(div().bg(hsla_u(BRAND).opacity(0.35)).child(part(from, to)))
                        })
                        .when(caret == to && from != to, |r| r.child(bar()))
                        .child(part(to, chars.len()))
                        .into_any_element()
                } else {
                    SharedString::from(text).into_any_element()
                };
                row.child(label)
                    .child(
                        boxed_with(shown)
                            .relative()
                            .child(probe(&self.probes, format!("dialog-field:{name}"))),
                    )
                    .when(editable, |r| {
                        r.cursor_text().on_click(cx.listener(
                            move |this, ev: &ClickEvent, window, cx| {
                                this.dialog_field_click(i, ev.position(), window, cx)
                            },
                        ))
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

/// How far in from the left of a field's `dialog-field` probe its text
/// starts: the box's `px_1` padding. The probe is absolutely positioned inside
/// the box's border, so the one-pixel border is already outside it.
const FIELD_INSET: f32 = 4.;

/// The character gap nearest to `x` pixels along `text`, whose prefixes
/// measure as `width` says.
fn char_at_x(text: &str, x: f32, width: impl Fn(&str) -> f32) -> usize {
    let ends: Vec<usize> = text.char_indices().map(|(i, c)| i + c.len_utf8()).collect();
    let mut prev = 0.0;
    for (n, &end) in ends.iter().enumerate() {
        let w = width(&text[..end]);
        if x < (prev + w) / 2.0 {
            return n;
        }
        prev = w;
    }
    ends.len()
}

/// The caret of a focused text field: a thin bar between two characters.
fn caret_bar(color: Hsla) -> Div {
    div().w(px(1.)).h(px(14.)).bg(color)
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
    fn chord() -> Modifiers {
        Modifiers {
            control: true,
            ..Modifiers::default()
        }
    }

    /// Ctrl+A selects the field's text and typing replaces it; Ctrl+V pastes
    /// the clipboard's text (its line breaks dropped); other chords still
    /// edit nothing (#1027).
    #[test]
    fn select_all_and_paste_edit_the_focused_field() {
        let mut t = tab_with_form();
        assert!(key(&mut t, "tab", None));
        assert!(key(&mut t, "tab", None));
        assert!(key(&mut t, "n", Some("n")));
        assert!(dialog_key_with(&mut t, "a", None, chord(), None));
        assert!(key(&mut t, "J", Some("J")));
        assert_eq!(value(&t, "note"), Value::Text("J".into()));
        assert!(dialog_key_with(
            &mut t,
            "v",
            None,
            chord(),
            Some("ane\r\nDoe")
        ));
        assert_eq!(value(&t, "note"), Value::Text("JaneDoe".into()));
        assert!(
            dialog_key_with(&mut t, "v", None, chord(), None),
            "an empty clipboard"
        );
        assert!(dialog_key_with(&mut t, "z", None, chord(), None));
        assert_eq!(value(&t, "note"), Value::Text("JaneDoe".into()));
    }

    /// Home, End, Left, Right and Delete move and edit within a field.
    #[test]
    fn arrows_home_end_and_delete_edit_in_place() {
        let mut t = tab_with_form();
        assert!(key(&mut t, "tab", None));
        assert!(key(&mut t, "tab", None));
        for c in ["a", "b", "c"] {
            assert!(key(&mut t, c, Some(c)));
        }
        assert!(key(&mut t, "home", None));
        assert!(key(&mut t, "right", None));
        assert!(key(&mut t, "delete", None));
        assert_eq!(value(&t, "note"), Value::Text("ac".into()));
        assert!(key(&mut t, "end", None));
        assert!(key(&mut t, "left", None));
        assert!(key(&mut t, "x", Some("x")));
        assert_eq!(value(&t, "note"), Value::Text("axc".into()));
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert!(dialog_key(&mut t, "home", None, shift));
        assert!(key(&mut t, "backspace", None));
        assert_eq!(
            value(&t, "note"),
            Value::Text("c".into()),
            "Shift+Home took 'ax'"
        );
    }

    /// A click at `x` lands the caret in the nearest gap between characters.
    #[test]
    fn a_click_maps_to_the_nearest_character_gap() {
        // Every character 10px wide.
        let w = |s: &str| s.chars().count() as f32 * 10.0;
        assert_eq!(char_at_x("abc", -4.0, w), 0);
        assert_eq!(char_at_x("abc", 4.0, w), 0);
        assert_eq!(char_at_x("abc", 6.0, w), 1);
        assert_eq!(char_at_x("abc", 14.0, w), 1);
        assert_eq!(char_at_x("abc", 16.0, w), 2);
        assert_eq!(char_at_x("abc", 29.0, w), 3);
        assert_eq!(char_at_x("abc", 500.0, w), 3);
        assert_eq!(char_at_x("", 5.0, w), 0);
        assert_eq!(
            char_at_x("a\u{1F600}", 19.0, w),
            2,
            "one character past a wide one"
        );
    }

    /// An app-level dialog stays the one shown when a tab becomes active
    /// under it, and the tab's own shows again once it closes (#1027).
    #[test]
    fn the_apps_dialog_is_not_stranded_by_a_tab_arriving() {
        let tab = tab_with_form();
        let mut app = DialogStack::default();
        assert!(shown_stack(&app, None).is_none(), "nothing open");
        assert_eq!(
            shown_stack(&app, Some(&tab.dialogs)).map(|s| s.top_id()),
            Some("form")
        );
        app.push(crate::user_name::dialog("", ""));
        assert_eq!(
            shown_stack(&app, None).map(|s| s.top_id()),
            Some("user-name")
        );
        assert_eq!(
            shown_stack(&app, Some(&tab.dialogs)).map(|s| s.top_id()),
            Some("user-name"),
            "a tab opened under it does not hide it"
        );
        app.clear();
        assert_eq!(
            shown_stack(&app, Some(&tab.dialogs)).map(|s| s.top_id()),
            Some("form")
        );
    }
}
