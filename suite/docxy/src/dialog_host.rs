//! The app side of [`crate::dialog`]: a tab's dialogs take keys and button
//! presses, an accept button applies the dialog to its owner, and the window
//! draws the top dialog over everything. The harness verbs and the drawn
//! buttons call the same [`dialog_click`].
use super::*;
use crate::dialog::{self, ControlKind, Dialog, DialogOwner, DialogStack, NONE_OPEN};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use unicode_segmentation::UnicodeSegmentation;

/// Each list of the top dialog, by the dialog's epoch and the control's
/// name: its scroll, and the choice last scrolled to.
pub(crate) type ListScrolls =
    std::collections::HashMap<(u64, &'static str), (UniformListScrollHandle, Option<usize>)>;

/// Each scrolling grid or filling multiline of the top dialog (#1609), by the
/// dialog's epoch and the control's name: its scroll, the row (or caret
/// line) last scrolled to, and the row height the offset counts in, so a
/// read-back (`first_visible_row` / `first_visible_line`) divides by what
/// the draw code laid out.
pub(crate) type GridScrolls = std::collections::HashMap<(u64, &'static str), GridScroll>;

/// A scrolling grid's or filling multiline's scroll, kept across frames
/// (#1609).
pub(crate) struct GridScroll {
    handle: ScrollHandle,
    /// The row last brought into view, with the mark that asked for it (a
    /// grid's row again; a multiline's caret and the dialog's edit count).
    seen: Option<(usize, u64)>,
    /// The row height it was drawn with.
    row_h: f32,
}

/// The first row (or line) a scrolling control shows, from its handle's
/// offset and the row height it was drawn with: the row nearest the top
/// edge (#1609). `None` while the control has nothing to scroll (its
/// content fits its box, as the handle's maximum offset after its last
/// paint says), so a filling multiline whose lines fit reports no line.
pub(crate) fn first_visible(entry: &GridScroll) -> Option<usize> {
    (f32::from(entry.handle.max_offset().y) > 0.).then(|| {
        (-f32::from(entry.handle.offset().y) / entry.row_h.max(1.))
            .round()
            .max(0.) as usize
    })
}

/// The scroll for grid or multiline `name` of the dialog opening `epoch`,
/// kept across frames; when `row` (the selected row, or the caret's line
/// with the caret and edit count as its mark) is not the one last seen, the
/// row is brought into view: the offset is set outright from the row height
/// and the view's height (`view_h`, else the element's last bounds), the
/// least that shows the row, as [`list_scroll`] does for a list. A wheel
/// scroll that leaves the row and its mark alone is not undone; an edit on
/// the line, the caret moved or not, brings the line back.
fn grid_scroll(
    grids: &mut GridScrolls,
    key: (u64, &'static str),
    row: Option<(usize, u64)>,
    row_h: f32,
    view_h: Option<f32>,
) -> ScrollHandle {
    let entry = grids.entry(key).or_insert_with(|| GridScroll {
        handle: ScrollHandle::new(),
        seen: None,
        row_h,
    });
    entry.row_h = row_h;
    if row != entry.seen {
        if let Some((r, _)) = row {
            let view = view_h
                .unwrap_or_else(|| f32::from(entry.handle.bounds().size.height))
                .max(row_h);
            let shown = -f32::from(entry.handle.offset().y);
            let (top, bottom) = (r as f32 * row_h, (r + 1) as f32 * row_h);
            let next = if top < shown {
                top
            } else if bottom > shown + view {
                bottom - view
            } else {
                shown
            };
            entry.handle.set_offset(point(px(0.), px(-next)));
        }
        entry.seen = row;
    }
    entry.handle.clone()
}

/// A 12 px text's estimated width from its character count, for the label
/// columns and the grid columns alike (#1609).
fn est_text_w(chars: usize) -> f32 {
    chars as f32 * 7. + 12.
}

/// The scroll for list `name` of the dialog opening `epoch`, kept across
/// frames. When the choice is not the one last seen it is scrolled into view
/// (only if it is out of it), so a key that moves it past the shown rows
/// brings it back, while a wheel scroll that leaves the choice alone is not
/// undone.
fn list_scroll(
    lists: &mut ListScrolls,
    key: (u64, &'static str),
    chosen: Option<usize>,
) -> UniformListScrollHandle {
    let (handle, seen) = lists
        .entry(key)
        .or_insert_with(|| (UniformListScrollHandle::new(), None));
    if chosen != *seen {
        if let Some(k) = chosen {
            handle.scroll_to_item(k, ScrollStrategy::Top);
        }
        *seen = chosen;
    }
    handle.clone()
}

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
        DialogOwner::TaskInfo { uid } => {
            let Surface::Project(v) = surface else {
                return Err("this dialog belongs to a Project".into());
            };
            // The tab's dirty flag follows the editor (`complete_project`).
            crate::project::apply_task_info(&mut v.ed, uid, dialog)?;
            Ok(false)
        }
        // Applied in `dialog_click`, which also puts the status on the tab.
        DialogOwner::Zoom | DialogOwner::Timescale => {
            Err("Zoom and Timescale apply through dialog_click".into())
        }
        // Handled in `plugin_ui::dialog_click`, before this: OK sends the
        // plugin its command again with the values.
        DialogOwner::Plugin { .. } => Err("a plugin's dialog applies through its OK".into()),
        DialogOwner::PageSetup | DialogOwner::Columns => {
            let (Surface::Doc(ed), Some(pkg)) = (surface, pkg) else {
                return Err("this dialog belongs to a .docx document".into());
            };
            match dialog.owner {
                DialogOwner::PageSetup => crate::page_setup::apply_page_setup(ed, pkg, dialog),
                _ => crate::page_setup::apply_columns(ed, pkg, dialog),
            }
        }
        DialogOwner::LineNumbers => {
            let Surface::Doc(ed) = surface else {
                return Err("this dialog belongs to a .docx document".into());
            };
            crate::page_setup::apply_line_numbers(ed, dialog)
        }
        DialogOwner::Hyphenation => {
            let (Surface::Doc(ed), Some(pkg)) = (surface, pkg) else {
                return Err("this dialog belongs to a .docx document".into());
            };
            crate::hyphenation_dialog::apply_hyphenation(ed, pkg, dialog)
        }
        // Handled in `manual_hyphen::click`, before this: Yes and No act and
        // ask about the next word.
        DialogOwner::ManualHyphen => Err("Manual hyphenation answers through its buttons".into()),
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
        DialogOwner::HfDateTime | DialogOwner::HfAlignmentTab => {
            // At the caret of the open header or footer, else the body.
            let ed = match (hf, surface) {
                (Some(ed), _) => ed,
                (None, Surface::Doc(ed)) => ed,
                _ => return Err("this dialog belongs to a document".into()),
            };
            match dialog.owner {
                DialogOwner::HfDateTime => crate::hf_insert::apply_date_time(ed, dialog),
                _ => crate::hf_insert::apply_alignment_tab(ed, dialog),
            }
        }
        // Handled in `Docxy::online_pictures_click`, before this.
        DialogOwner::HfOnlinePictures => Err("Online Pictures applies through Pictures...".into()),
        // Their presses are handled in `ttc_dialog::click`, before this.
        DialogOwner::TextToColumns { .. } | DialogOwner::TextToColumnsReplace => {
            Err("Text to Columns applies through its own wizard".into())
        }
        // Handled in `sheet_goto::click`, before this.
        DialogOwner::GoTo | DialogOwner::GoToSpecial => {
            Err("Go To applies through Find & Select".into())
        }
        // Handled in `sheet_audit::click`, before this.
        DialogOwner::ErrorChecking { .. }
        | DialogOwner::ErrorCheckDone
        | DialogOwner::EvaluateFormula => Err("Error Checking applies through Formulas".into()),
        // Handled in `Docxy::paste_dialog_click`, before this.
        DialogOwner::PasteSpecial { .. } => Err("Paste Special applies through Paste".into()),
        // Handled in `Docxy::doc_paste_dialog_click`, before this.
        DialogOwner::DocPasteSpecial => Err("Paste Special applies through Paste".into()),
        // Handled in `doc_fields::click`, before this.
        DialogOwner::EditField => Err("Edit Field applies through the field keys".into()),
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
        // Handled in `sheet_format_cells::click`, before this.
        DialogOwner::FormatCells { .. } => Err("Format Cells applies through its OK".into()),
        // Handled in `sheet_filter::click` and `sheet_sort::click`, before this.
        DialogOwner::FilterMenu { .. }
        | DialogOwner::CustomFilter { .. }
        | DialogOwner::Top10Filter { .. }
        | DialogOwner::AdvancedFilter { .. } => Err("a filter applies through the Data tab".into()),
        // Handled in `project::autofilter_click`, before this.
        DialogOwner::ProjectFilter { .. } => Err("a filter applies through its OK".into()),
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
        // Handled in `sheet_calc::click`, before this.
        DialogOwner::SheetCalcOptions | DialogOwner::SheetPrecisionWarning => {
            Err("calculation options apply through the Formulas tab".into())
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
        // Handled in `doc_recovery::click`, before this: it replaces tabs.
        DialogOwner::DocumentRecovery => Err("document recovery applies through the app".into()),
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
                | DialogOwner::DocumentRecovery
                | DialogOwner::About
                | DialogOwner::SaveOnClose { .. }
                | DialogOwner::Series
                | DialogOwner::JustifyOverflow
                | DialogOwner::CustomLists
                | DialogOwner::PasteSpecial { .. }
                | DialogOwner::DocPasteSpecial
                | DialogOwner::DropReplace
                | DialogOwner::GoTo
                | DialogOwner::GoToSpecial
                | DialogOwner::ErrorChecking { .. }
                | DialogOwner::ErrorCheckDone
                | DialogOwner::EvaluateFormula
                | DialogOwner::HfOnlinePictures
        )
}

/// Whether the active tab's top dialog is the reopen question, whose Yes
/// replaces the tab under any grid state the window keeps for it.
fn reopen_on_top(tab: Option<&DocTab>) -> bool {
    tab.and_then(|t| t.dialogs.top())
        .is_some_and(|d| matches!(d.owner, DialogOwner::Reopen { .. }))
}

/// Press a button on the tab's top dialog, by its label. A press that made
/// a new circular reference (Consolidate's links, Subtotal, a sort, a typed
/// entry the validation alert lets in) warns after the dialog's own status.
pub(crate) fn dialog_click(tab: &mut DocTab, button: &str) -> Result<(), String> {
    let begun = tab.circle_cmd_begin();
    let done = dialog_press(tab, button);
    tab.circle_cmd_end(begun);
    done
}

fn dialog_press(tab: &mut DocTab, button: &str) -> Result<(), String> {
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
    // Formulas' More Calculation Options and its precision warning (#674).
    if let Some(done) = crate::sheet_calc::click(tab, button) {
        return done;
    }
    // Go To's OK selects; its Special… opens Go To Special (#671).
    if let Some(done) = crate::sheet_goto::click(tab, button) {
        return done;
    }
    // Error Checking's presses (Next, Previous, Ignore Error, ...) and Evaluate
    // Formula's (Evaluate, Step In, ...) (#675).
    if let Some(done) = crate::sheet_audit::click(tab, button) {
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
    // Format Cells (#1165).
    if let Some(done) = crate::sheet_format_cells::click(tab, button) {
        return done;
    }
    // The filter drop-down and its dialogs (#690), and the sorts (#691).
    if let Some(done) = crate::sheet_filter::click(tab, button) {
        return done;
    }
    if let Some(done) = crate::sheet_sort::click(tab, button) {
        return done;
    }
    // The Project table's AutoFilter drop-down (#1533).
    if let Some(done) = crate::project::autofilter_click(tab, button) {
        return done;
    }
    if let Some(done) = reopen_click(tab, button) {
        return done;
    }
    if let Some(done) = crate::mailings_dialogs::click(tab, button) {
        return done;
    }
    if let Some(done) = crate::manual_hyphen::click(tab, button) {
        return done;
    }
    if let Some(done) = crate::doc_fields::click(tab, button) {
        return done;
    }
    if let Some(done) = crate::design_dialogs::click(tab, button) {
        return done;
    }
    // Task Information's Help (#1536).
    if let Some(done) = crate::project::help_click(tab, button) {
        return done;
    }
    // A UI plugin's dialog, or Task Information with plugin pages (#1532).
    if let Some(done) = crate::plugin_ui::dialog_click(tab, button) {
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
    let mut status = None;
    dialogs.click(button, |d| {
        // The Gantt's zoom is view state: its OK says the new timescale.
        if matches!(d.owner, DialogOwner::Zoom | DialogOwner::Timescale) {
            let Surface::Project(v) = surface else {
                return Err("this dialog belongs to a Project".into());
            };
            status = Some(if d.owner == DialogOwner::Zoom {
                crate::project::apply_zoom(v, d)?
            } else {
                crate::project::apply_timescale(v, d)?
            });
            return Ok(());
        }
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
    if let Some(status) = status {
        tab.status = status.into();
    }
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
    // A modeless dialog that is not active leaves the keys to the document
    // (#1602).
    if !tab.dialogs.top_takes_keys() {
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
    // A plugin control typed into: its state is asked again (#1602).
    crate::plugin_ui::refresh_dialog(tab);
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
        // Alt+Down drops a date combo's calendar down, as in Windows (#1536).
        if key == "down"
            && let Some(i) = d
                .focused()
                .filter(|c| c.kind == dialog::ControlKind::DateCombo)
                .and(d.focus)
        {
            return d.toggle_calendar(i);
        }
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
                // A multiline field keeps the pasted text's line breaks.
                let multiline = d
                    .focused()
                    .is_some_and(|c| c.kind == dialog::ControlKind::Multiline);
                let line: String = text
                    .replace("\r\n", "\n")
                    .chars()
                    .filter(|c| !c.is_control() || (multiline && *c == '\n'))
                    .collect();
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
    // F4 (Alt+Down was returned above) drops a date combo's calendar down;
    // an open one takes the arrows, Page Up and Down, Enter and Escape (#1536).
    if d.calendar_key(key)? {
        return Ok(());
    }
    if key == "f4"
        && let Some(i) = d
            .focused()
            .filter(|c| c.kind == dialog::ControlKind::DateCombo)
            .and(d.focus)
    {
        return d.toggle_calendar(i);
    }
    // A focused grid takes the arrows, Space, typing, Enter and Escape of a
    // cell edit, Backspace and Delete (#1602).
    if d.grid_key(key, typed.as_deref())? {
        return Ok(());
    }
    match key {
        "tab" => {
            d.commit_pending()?;
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
            if !d.list_edge(key == "end")? {
                d.move_caret_edge(key == "end", m.shift);
            }
            Ok(())
        }
        "space" => d.space(),
        // Enter reaches here only with a multiline field focused
        // (`DialogStack::key_button`): a new line (#1532).
        "enter" => d.insert_text("\n"),
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
        // A modeless dialog covers nothing (#1602).
        match self
            .active_dialogs()
            .filter(|s| s.modal_open())
            .and_then(|d| d.top())
        {
            Some(d) => Err(format!("a dialog is open: {}", d.title)),
            None => Ok(()),
        }
    }

    /// Ask the plugin's state again for the active tab's dialog when it is
    /// stale or the plan or the selection moved under it (#1602, #1610):
    /// before a reply is written.
    pub(crate) fn refresh_plugin_dialog(&mut self) {
        if !self.app_dialogs.is_open()
            && let Some(tab) = self.tabs.get_mut(self.active)
        {
            crate::plugin_ui::refresh_dialog(tab);
        }
    }

    /// A press in the open dialog gives it the keyboard (a modeless one may
    /// have lost it to the document), then the plugin's state is asked again
    /// when a control of its changed (#1602).
    fn dialog_touched(&mut self) {
        self.active_dialogs_mut().activate(true);
        if !self.app_dialogs.is_open()
            && let Some(tab) = self.tabs.get_mut(self.active)
        {
            crate::plugin_ui::refresh_dialog(tab);
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
        // A plugin dialog's Help is the host's (#1609): it opens the spec's
        // help page, or says the documentation is coming; the dialog stays.
        if let Some(done) = self.plugin_help_click(button, cx) {
            return done;
        }
        // So is Document Recovery, which replaces tabs (#1225).
        if let Some(done) = self.document_recovery_click(button, cx) {
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
        // And a document's, which pastes what the clipboard holds (#753).
        if let Some(done) = self.doc_paste_dialog_click(button, window, cx) {
            return done;
        }
        // And a drop by the selection's border waiting on its question (#670).
        if let Some(done) = self.drop_dialog_click(button) {
            return done;
        }
        // Online Pictures' From a File... opens the native file dialog (#768).
        if let Some(done) = self.online_pictures_click(button, window, cx) {
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
                // A document copy's rich clip no longer stands for what
                // the clipboard holds, even when the text matches.
                self.clip = None;
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
        self.dialog_touched();
        self.refocus(window, cx);
    }

    /// The top dialog's Help pressed on a plugin dialog (#1609): the spec's
    /// `help` URL opens (recorded for `last-url`), else the status line says
    /// the documentation is coming, as the Help tab does. `None` for any
    /// other dialog or button.
    fn plugin_help_click(
        &mut self,
        button: &str,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let dialogs = self.active_dialogs()?;
        let top = dialogs.top()?;
        let DialogOwner::Plugin { spec, .. } = top.owner else {
            return None;
        };
        let pressed = top.find_button(button)?;
        if !pressed.id.eq_ignore_ascii_case("help") {
            return None;
        }
        if let Err(e) = Dialog::refuse_disabled(pressed) {
            return Some(Err(e));
        }
        match crate::plugin_ui::help_url(spec) {
            Some(url) => self.open_help_url(url, cx),
            None => self.set_status(crate::help_tab::DOCS_COMING.to_string()),
        }
        Some(Ok(()))
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
        self.dialog_touched();
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
        // A multiline's probe is its first line's neighbour and scrolls with
        // the lines (#1609), so the distance down from it is the line, the
        // scroll already counted.
        let y = f32::from(at.y - bounds.top());
        let measurer = Measurer::new(window);
        if let Some(d) = self
            .active_dialogs_mut()
            .top_dialog_mut()
            .ok()
            .filter(|d| d.focus == Some(index))
        {
            let (text, multiline) = d
                .focused()
                .map(|c| (c.text(), c.kind == ControlKind::Multiline))
                .unwrap_or_default();
            let to = if multiline {
                char_at_line_x(&text, y, x, |s| measurer.width(s, 12., false, false))
            } else {
                char_at_x(&text, x, |s| measurer.width(s, 12., false, false))
            };
            d.move_caret_to(to, false);
        }
        cx.notify();
    }

    /// A press in the calendar of date combo `index` (#1536).
    fn dialog_calendar(&mut self, index: usize, act: CalendarAct, cx: &mut Context<Self>) {
        let done = self
            .active_dialogs_mut()
            .top_dialog_mut()
            .and_then(|d| match act {
                CalendarAct::Toggle => d.toggle_calendar(index),
                CalendarAct::Month(n) => d.calendar_move(n, true),
                CalendarAct::Day(day) => d.calendar_day(day),
            });
        if let Err(e) = done {
            self.set_status(e);
        }
        self.dialog_touched();
        cx.notify();
    }

    /// The month a date combo's calendar shows: its title between the
    /// previous and next month buttons, the weekday headings, and the days,
    /// the cursor's shaded and today's outlined.
    fn calendar_popup(
        &self,
        i: usize,
        name: &'static str,
        k: dialog::calendar::Calendar,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let today = dialog::calendar::today();
        let step = |id: &'static str, label: &'static str, by: i32, cx: &mut Context<Self>| {
            div()
                .id((id, i))
                .relative()
                .px_2()
                .cursor_pointer()
                .child(probe(&self.probes, format!("dialog-calendar:{id}:{name}")))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dialog_calendar(i, CalendarAct::Month(by), cx)
                }))
        };
        let mut rows = Vec::new();
        for (w, week) in k.weeks().into_iter().enumerate() {
            let mut days = Vec::new();
            for (n, day) in week.into_iter().enumerate() {
                let cell = div()
                    .relative()
                    .w(px(26.))
                    .h(px(18.))
                    .flex()
                    .justify_center();
                days.push(match day {
                    None => cell.into_any_element(),
                    Some(day) => cell
                        .id(("dialog-calendar-day", (i * 8 + w) * 8 + n))
                        .cursor_pointer()
                        .when(day == k.day(), |c| c.bg(hsla_u(BRAND).opacity(0.35)))
                        .when(k.is_today(day, today), |c| {
                            c.border_1().border_color(hsla_u(BRAND))
                        })
                        .child(probe(
                            &self.probes,
                            format!("dialog-calendar:day:{name}:{day}"),
                        ))
                        .child(SharedString::from(day.to_string()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dialog_calendar(i, CalendarAct::Day(day), cx)
                        }))
                        .into_any_element(),
                });
            }
            rows.push(h_flex().children(days));
        }
        v_flex()
            .id(("dialog-calendar", i))
            .relative()
            .p_1()
            .gap_1()
            .border_1()
            .border_color(pal.dim)
            .bg(pal.panel)
            .child(probe(&self.probes, format!("dialog-calendar:popup:{name}")))
            .child(
                h_flex()
                    .justify_between()
                    .child(step("prev", "\u{25C0}", -1, cx))
                    .child(SharedString::from(k.title()))
                    .child(step("next", "\u{25B6}", 1, cx)),
            )
            .child(
                h_flex().children(dialog::calendar::WEEKDAYS.iter().map(|w| {
                    div()
                        .w(px(26.))
                        .flex()
                        .justify_center()
                        .text_color(pal.dim)
                        .child(*w)
                })),
            )
            .children(rows)
            .into_any_element()
    }

    /// A press on the top dialog's tab strip.
    fn dialog_tab_click(&mut self, label: &str, cx: &mut Context<Self>) {
        if let Err(e) = self.active_dialogs_mut().select_tab(label) {
            self.set_status(e);
        }
        self.dialog_touched();
        cx.notify();
    }

    /// One control's widget: a field, checkbox, radio group, dropdown or list
    /// that takes the pointer, a grid, a label drawn as text, or a sample box
    /// drawn in the style its value names (#1609). `label_w` is the label's
    /// width when the page's columns align labels, `None` for its natural
    /// width.
    fn dialog_widget(
        &self,
        d: &Dialog,
        i: usize,
        label_w: Option<f32>,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = &d.controls[i];
        let focused = d.focus == Some(i) && d.focused().is_some();
        let fg = if c.enabled { pal.fg } else { pal.dim };
        let label = SharedString::from(c.label.replace('&', ""));
        // On a page of several columns the labels of one column share a
        // width, so the fields line up (#1609).
        let aligned = |label: SharedString| -> AnyElement {
            match label_w {
                Some(w) => div()
                    .w(px(w))
                    .flex_none()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(label)
                    .into_any_element(),
                None => label.into_any_element(),
            }
        };
        // A multiline that fills the page (#1609) in a dialog of fixed size.
        let fill = d.fills(c);
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
            // A multiline field (#1532): the lines one under another, the
            // caret's line drawn with its caret and selection like a field's.
            ControlKind::Multiline => {
                let name = c.name;
                let text = c.text();
                let chars: Vec<char> = text.chars().collect();
                let caret = d.caret_at().min(chars.len());
                let (from, to) = d.selection().unwrap_or((caret, caret));
                let mut lines: Vec<AnyElement> = Vec::new();
                let mut start = 0;
                let mut caret_line = 0;
                let line_count = text.split('\n').count();
                for (n, line) in text.split('\n').enumerate() {
                    let len = line.chars().count();
                    let end = start + len;
                    let on_line =
                        caret >= start && caret <= end && (n + 1 == line_count || caret < end + 1);
                    if on_line {
                        caret_line = n;
                    }
                    let shown: AnyElement = if focused && on_line {
                        let part = |a: usize, b: usize| {
                            SharedString::from(
                                chars[a.min(end)..b.min(end)].iter().collect::<String>(),
                            )
                        };
                        let (f, t) = (from.clamp(start, end), to.clamp(start, end));
                        h_flex()
                            .h(px(16.))
                            .child(part(start, f))
                            .when(caret == f, |r| r.child(caret_bar(pal.fg)))
                            .when(f != t, |r| {
                                r.child(div().bg(hsla_u(BRAND).opacity(0.35)).child(part(f, t)))
                            })
                            .when(caret == t && f != t, |r| r.child(caret_bar(pal.fg)))
                            .child(part(t, end))
                            .into_any_element()
                    } else {
                        div()
                            .h(px(16.))
                            .child(SharedString::from(line.to_string()))
                            .into_any_element()
                    };
                    lines.push(shown);
                    start = end + 1;
                }
                // The lines are 16 px each; the box's height is the frame's.
                // Only this box's own caret, while it is focused, drives the
                // scroll (`caret_at` is the focused control's); unfocused it
                // keeps its offset. The caret and the edit count are the
                // mark, so typing on, or deleting from, a line a wheel
                // scrolled away brings the line back.
                let scroll = fill.then(|| {
                    grid_scroll(
                        &mut self.dialog_grids.borrow_mut(),
                        (d.epoch, c.name),
                        focused.then_some((caret_line, (d.edits << 32) | caret as u64)),
                        16.,
                        None,
                    )
                });
                if !fill {
                    self.dialog_grids.borrow_mut().remove(&(d.epoch, c.name));
                }
                v_flex()
                    .id(("dialog-control", i))
                    .gap_1()
                    .text_size(px(12.))
                    .text_color(fg)
                    .when(fill, |v| v.flex_1().min_h_0().w_full())
                    .child(probe(&self.probes, format!("dialog-control:{}", c.name)))
                    .when(!label.is_empty(), |v| v.child(label))
                    .child(
                        div()
                            .relative()
                            .when(fill, |v| v.flex_1().min_h_0().w_full())
                            .child(
                                v_flex()
                                    .id(("dialog-multiline", i))
                                    .relative()
                                    .min_w(px(240.))
                                    .min_h(px(16. * 3. + 4.))
                                    // A filling box scrolls its lines, the
                                    // caret's kept in view (#1609).
                                    .when_some(scroll.clone(), |v, h| {
                                        v.h_full().w_full().overflow_y_scroll().track_scroll(&h)
                                    })
                                    .px_1()
                                    .border_1()
                                    .border_color(if focused { hsla_u(BRAND) } else { pal.dim })
                                    .bg(pal.panel)
                                    .child(probe(&self.probes, format!("dialog-field:{name}")))
                                    .children(lines)
                                    .when(editable, |r| {
                                        r.cursor_text().on_click(cx.listener(
                                            move |this, ev: &ClickEvent, window, cx| {
                                                this.dialog_field_click(
                                                    i,
                                                    ev.position(),
                                                    window,
                                                    cx,
                                                )
                                            },
                                        ))
                                    }),
                            )
                            .when_some(scroll, |b, h| {
                                b.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom_0()
                                        .w(px(8.))
                                        .child(
                                            Scrollbar::vertical(&h)
                                                .id(SharedString::from(format!(
                                                    "dialog-multiline-scroll:{name}"
                                                )))
                                                .scrollbar_show(ScrollbarShow::Always),
                                        ),
                                )
                            }),
                    )
                    .into_any_element()
            }
            // A grid (#1532): its headings and rows as a table; a click
            // selects a row and its cell (a check cell toggles), the
            // selected row is shaded, the active cell outlined, and a cell
            // being typed into shows its text with a caret (#1602).
            ControlKind::Grid => {
                let rows = match &c.value {
                    dialog::Value::Rows(rows) => rows.clone(),
                    _ => Vec::new(),
                };
                let (sel_row, sel_col, editing) = (c.row, c.col, c.edit.clone());
                let widths: Vec<f32> = (0..c.columns.len())
                    .map(|col| {
                        let longest = std::iter::once(c.columns[col].chars().count())
                            .chain(
                                rows.iter()
                                    .map(|r| r.get(col).map_or(0, |s| s.chars().count())),
                            )
                            .max()
                            .unwrap_or(1);
                        let floor = if c.editable_grid { 56. } else { 40. };
                        est_text_w(longest).clamp(floor, 220.)
                    })
                    .collect();
                let cell = |content: AnyElement, w: f32, head: bool, active: bool| {
                    div()
                        .relative()
                        .w(px(w))
                        .h(px(18.))
                        .px_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .border_r_1()
                        .border_b_1()
                        .border_color(pal.border)
                        .when(head, |d| d.font_weight(FontWeight::BOLD).bg(pal.hover))
                        .when(active, |d| d.border_1().border_color(hsla_u(BRAND)))
                        .child(content)
                };
                let text_cell = |text: String| SharedString::from(text).into_any_element();
                let header =
                    h_flex().children(c.columns.iter().enumerate().map(|(col, name)| {
                        cell(text_cell(name.clone()), widths[col], true, false)
                    }));
                // At least `min_rows` rows are drawn, empty ones filling
                // in; more rows scroll (#1609). A built-in dialog's grid
                // keeps its own height unless it asks.
                let min_rows = c.min_rows.unwrap_or(if c.plugin { 8 } else { rows.len() });
                let fillers = (rows.len()..min_rows).map(|r| {
                    h_flex().id(("dialog-grid-filler", i * 1024 + r)).children(
                        (0..c.columns.len())
                            .map(|col| cell(text_cell(String::new()), widths[col], false, false)),
                    )
                });
                let body = rows.iter().enumerate().map(|(r, row)| {
                    let selected = sel_row == Some(r);
                    h_flex()
                        .id(("dialog-grid-row", i * 1024 + r))
                        .when(selected, |h| h.bg(pal.hover))
                        .children((0..c.columns.len()).map(|col| {
                            let text = row.get(col).cloned().unwrap_or_default();
                            let active = selected && sel_col == col && focused;
                            let content: AnyElement = match (c.column_kind(col), text.as_str()) {
                                (ControlKind::Checkbox, "true") => text_cell("\u{2611}".into()),
                                (ControlKind::Checkbox, _) => text_cell("\u{2610}".into()),
                                // Project's check mark column: a mark or nothing.
                                (ControlKind::Check, "true") => text_cell("\u{2713}".into()),
                                (ControlKind::Check, _) => text_cell(String::new()),
                                (kind, _) => match (&editing, active) {
                                    (Some(buf), true) => h_flex()
                                        .child(SharedString::from(buf.clone()))
                                        .child(caret_bar(pal.fg))
                                        .into_any_element(),
                                    _ if kind == ControlKind::Dropdown
                                        && c.editable_grid
                                        && selected =>
                                    {
                                        text_cell(format!("{text} \u{25BE}"))
                                    }
                                    _ => text_cell(text),
                                },
                            };
                            cell(content, widths[col], false, active)
                                .child(probe(
                                    &self.probes,
                                    format!(
                                        "dialog-cell:{}[{r}].{}",
                                        c.name,
                                        c.columns[col].to_lowercase()
                                    ),
                                ))
                                .id(("dialog-grid-cell", (i * 1024 + r) * 256 + col))
                                .when(c.enabled && (c.plugin || c.editable_grid), |d| {
                                    d.cursor_pointer().on_click(on_widget(
                                        cx,
                                        i,
                                        Some(r * 256 + col),
                                    ))
                                })
                        }))
                });
                let scrolls = rows.len() > min_rows;
                let body_h = 18. * min_rows as f32;
                let scroll = scrolls.then(|| {
                    grid_scroll(
                        &mut self.dialog_grids.borrow_mut(),
                        (d.epoch, c.name),
                        sel_row.map(|r| (r, r as u64)),
                        18.,
                        Some(body_h),
                    )
                });
                // A grid that stopped scrolling (rows deleted under it) reports
                // no scroll.
                if !scrolls {
                    self.dialog_grids.borrow_mut().remove(&(d.epoch, c.name));
                }
                // The items a combo cell being typed into offers (#1536).
                let suggestions = c.suggestions();
                let offered = c.column_items.get(sel_col).cloned().unwrap_or_default();
                let pick = c.pick;
                let name = c.name;
                v_flex()
                    .id(("dialog-control", i))
                    .gap_1()
                    .text_size(px(12.))
                    .text_color(fg)
                    .child(probe(&self.probes, format!("dialog-control:{}", c.name)))
                    .when(!label.is_empty(), |v| v.child(label))
                    .child(
                        v_flex()
                            .border_1()
                            .border_color(if focused { hsla_u(BRAND) } else { pal.dim })
                            .bg(pal.panel)
                            .child(header)
                            .child(
                                div()
                                    .relative()
                                    .child(
                                        v_flex()
                                            .id(("dialog-grid-body", i))
                                            .when_some(scroll.clone(), |b, h| {
                                                b.h(px(body_h)).overflow_y_scroll().track_scroll(&h)
                                            })
                                            .children(body)
                                            .children(fillers),
                                    )
                                    // The scrollbar Project draws beside a grid
                                    // that holds more rows than it shows.
                                    .when_some(scroll, |b, h| {
                                        b.child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .right_0()
                                                .h(px(body_h))
                                                .w(px(8.))
                                                .child(
                                                    Scrollbar::vertical(&h)
                                                        .id(SharedString::from(format!(
                                                            "dialog-grid-scroll:{}",
                                                            c.name
                                                        )))
                                                        .scrollbar_show(ScrollbarShow::Always),
                                                ),
                                        )
                                    }),
                            ),
                    )
                    .when(!suggestions.is_empty(), |v| {
                        v.child(
                            v_flex()
                                .border_1()
                                .border_color(pal.dim)
                                .bg(pal.panel)
                                .max_h(px(18. * 6.))
                                .overflow_hidden()
                                .child(probe(&self.probes, format!("dialog-suggestions:{name}")))
                                .children(
                                    suggestions
                                        .iter()
                                        .take(dialog::SUGGESTIONS_DRAWN)
                                        .enumerate()
                                        .map(|(k, &n)| {
                                            h_flex()
                                                .id(("dialog-suggestion", i * 1024 + k))
                                                .h(px(18.))
                                                .px_1()
                                                .cursor_pointer()
                                                .when(pick == Some(k), |r| {
                                                    r.bg(hsla_u(BRAND).opacity(0.35))
                                                })
                                                .relative()
                                                .child(probe(
                                                    &self.probes,
                                                    format!("dialog-cell:{name}[suggest].{k}"),
                                                ))
                                                .child(SharedString::from(offered[n].clone()))
                                                .on_click(on_widget(
                                                    cx,
                                                    i,
                                                    Some(dialog::SUGGESTION_BASE + k),
                                                ))
                                        }),
                                ),
                        )
                    })
                    .into_any_element()
            }
            // A sample box (#1609): the text in the style the state gives, the
            // formatting model's text style plus `text`.
            ControlKind::Sample => {
                // The one parse `dialog-read` reports too (`Control::sample`),
                // re-read as the formatting model's JSON for its text style.
                let sample = c.sample();
                let text = sample.get_str("text").unwrap_or("AaBbYyZz").to_string();
                let style = projcore::json::Json::parse(&sample.to_string())
                    .map(|j| projcore::formatting::TextStyle::from_json(&j))
                    .unwrap_or_default();
                v_flex()
                    .id(("dialog-control", i))
                    .gap_1()
                    .text_size(px(12.))
                    .text_color(fg)
                    .w_full()
                    .child(probe(&self.probes, format!("dialog-control:{}", c.name)))
                    .when(!label.is_empty(), |v| v.child(label))
                    .child(
                        div()
                            .min_h(px(48.))
                            .w_full()
                            .p_2()
                            .border_1()
                            .border_color(pal.dim)
                            .bg(pal.panel)
                            .child(
                                crate::plugin_ui::styled(div(), &style)
                                    .child(SharedString::from(text)),
                            ),
                    )
                    .into_any_element()
            }
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
                let combo = k == ControlKind::DateCombo;
                let calendar = d.calendar.filter(|k| k.control == i);
                let field = row
                    .child(aligned(label))
                    .child(
                        boxed_with(shown)
                            .relative()
                            .when(label_w.is_some(), |b| b.flex_1())
                            .child(probe(&self.probes, format!("dialog-field:{name}"))),
                    )
                    .when(combo, |r| {
                        r.child(
                            div()
                                .id(("dialog-calendar-button", i))
                                .relative()
                                .px_1()
                                .border_1()
                                .border_color(pal.dim)
                                .child(probe(
                                    &self.probes,
                                    format!("dialog-calendar:button:{name}"),
                                ))
                                .child("\u{25BE}")
                                .when(editable, |b| {
                                    b.cursor_pointer().on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.dialog_calendar(i, CalendarAct::Toggle, cx)
                                        },
                                    ))
                                }),
                        )
                    })
                    .when(editable, |r| {
                        r.cursor_text().on_click(cx.listener(
                            move |this, ev: &ClickEvent, window, cx| {
                                this.dialog_field_click(i, ev.position(), window, cx)
                            },
                        ))
                    });
                match calendar {
                    Some(k) => v_flex()
                        .gap_1()
                        .child(field)
                        .child(self.calendar_popup(i, name, k, pal, cx))
                        .into_any_element(),
                    None => field.into_any_element(),
                }
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
                row.child(aligned(label))
                    .when(focused, |r| r.border_b_1().border_color(hsla_u(BRAND)))
                    .children(items)
                    .into_any_element()
            }
            ControlKind::Dropdown => row
                .child(aligned(label))
                .child(
                    boxed(format!("{} \u{25BE}", c.text())).when(label_w.is_some(), |b| b.flex_1()),
                )
                .when(editable, |r| {
                    r.cursor_pointer().on_click(on_widget(cx, i, None))
                })
                .into_any_element(),
            ControlKind::CheckList => {
                // Virtualised: an AutoFilter list holds up to 10,000 values.
                let n = c.items.len();
                let scroll =
                    list_scroll(&mut self.dialog_lists.borrow_mut(), (d.epoch, c.name), None);
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
                .track_scroll(&scroll)
                .h(px(d.popup.map_or(220., |p| p.list_h)))
                .w_full();
                v_flex()
                    .id(("dialog-control", i))
                    .gap_1()
                    .text_size(px(12.))
                    .text_color(fg)
                    .child(probe(&self.probes, format!("dialog-control:{}", c.name)))
                    .when(d.popup.is_none(), |e| e.child(label))
                    .child(
                        div()
                            .border_1()
                            .border_color(if focused { hsla_u(BRAND) } else { pal.dim })
                            .bg(pal.panel)
                            .relative()
                            .child(list)
                            .when(d.popup.is_some(), |b| {
                                b.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom_0()
                                        .w(px(12.))
                                        .child(
                                            Scrollbar::vertical(&scroll)
                                                .id(("dialog-checklist-bar", i))
                                                .scrollbar_show(ScrollbarShow::Always),
                                        ),
                                )
                            }),
                    )
                    .into_any_element()
            }
            ControlKind::List => {
                // One row per item, the chosen one shaded; a click chooses it
                // (#988). Virtualised like the check list: Go To's names can
                // run long.
                let n = c.items.len();
                let chosen = match c.value {
                    dialog::Value::Choice(k) => k,
                    _ => None,
                };
                let scroll = list_scroll(
                    &mut self.dialog_lists.borrow_mut(),
                    (d.epoch, c.name),
                    chosen,
                );
                let list = uniform_list(
                    ("dialog-list", i),
                    n,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        let Some(c) = this
                            .active_dialogs()
                            .and_then(|s| s.top())
                            .and_then(|d| d.controls.get(i))
                        else {
                            return vec![];
                        };
                        let chosen = match c.value {
                            dialog::Value::Choice(k) => k,
                            _ => None,
                        };
                        let enabled = c.enabled;
                        range
                            .filter(|&k| k < c.items.len())
                            .map(|k| {
                                h_flex()
                                    .id(("dialog-list-item", k))
                                    .h(px(18.))
                                    .px_1()
                                    .when(chosen == Some(k), |r| r.bg(hsla_u(BRAND).opacity(0.35)))
                                    .child(SharedString::from(c.items[k].clone()))
                                    .when(enabled, |r| {
                                        r.cursor_pointer().on_click(on_widget(cx, i, Some(k)))
                                    })
                                    .into_any_element()
                            })
                            .collect()
                    }),
                )
                .track_scroll(&scroll)
                .h(px(18. * n.clamp(3, 8) as f32))
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
                            .min_w(px(160.))
                            .border_1()
                            .border_color(if focused { hsla_u(BRAND) } else { pal.dim })
                            .bg(pal.panel)
                            .child(list),
                    )
                    .into_any_element()
            }
            // A plugin label with no caption draws its text alone; a built-in
            // one keeps the empty caption's indent.
            _ => row
                .when(!(c.plugin && label.is_empty()), |r| r.child(aligned(label)))
                .child(SharedString::from(c.text()))
                .into_any_element(),
        }
    }

    /// The controls of the top dialog drawn as [`Dialog::layout_rows`] lays
    /// them out (#1609): the header band first, each titled group in a box,
    /// a row of several controls side by side with the labels of a column
    /// sharing a width, a filling multiline's row and band growing.
    fn dialog_body(&self, d: &Dialog, pal: Pal, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        for band in d.layout_rows() {
            let columns = band.columns;
            // The label width per column: the longest label in it.
            let mut label_w = vec![0f32; columns];
            if columns > 1 {
                for row in &band.rows {
                    for &(i, col) in row {
                        let c = &d.controls[i];
                        if !matches!(
                            c.kind,
                            ControlKind::Multiline
                                | ControlKind::Grid
                                | ControlKind::List
                                | ControlKind::Sample
                                | ControlKind::CheckList
                                | ControlKind::Checkbox
                        ) {
                            let w = est_text_w(c.label.replace('&', "").chars().count()) - 8.;
                            label_w[col] = label_w[col].max(w);
                        }
                    }
                }
            }
            let band_fills = band
                .rows
                .iter()
                .flatten()
                .any(|&(i, _)| d.fills(&d.controls[i]));
            let mut drawn: Vec<AnyElement> = Vec::new();
            for row in band.rows {
                let fills = row.iter().any(|&(i, _)| d.fills(&d.controls[i]));
                if row.len() == 1 && (columns == 1 || d.controls[row[0].0].span as usize >= columns)
                {
                    let (i, _) = row[0];
                    // A control spanning a page of several columns keeps the
                    // first column's label width, so its field lines up with
                    // the fields above and fills the row (#1609 r6).
                    let w = (columns > 1).then(|| label_w[0]).filter(|w| *w > 0.);
                    let el = self.dialog_widget(d, i, w, pal, cx);
                    drawn.push(if fills {
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .child(el)
                            .into_any_element()
                    } else {
                        el
                    });
                    continue;
                }
                if row.len() > columns {
                    // Controls marked `beside` share the row of the one before
                    // them (#1536): Duration with Estimated, Start with Finish.
                    let widgets: Vec<AnyElement> = row
                        .into_iter()
                        .map(|(i, _)| self.dialog_widget(d, i, None, pal, cx))
                        .collect();
                    drawn.push(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .children(widgets)
                            .into_any_element(),
                    );
                    continue;
                }
                let cells = row.into_iter().map(|(i, col)| {
                    let span = (d.controls[i].span as usize).clamp(1, columns);
                    let w = Some(label_w[col]).filter(|w| *w > 0.);
                    v_flex()
                        .w(relative(span as f32 / columns as f32))
                        .pr_2()
                        .when(fills, |c| c.h_full())
                        .child(self.dialog_widget(d, i, w, pal, cx))
                });
                drawn.push(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .when(fills, |r| r.flex_1().min_h_0())
                        .children(cells)
                        .into_any_element(),
                );
            }
            match band.title {
                Some(t) => out.push(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .p_2()
                        .border_1()
                        .border_color(pal.border)
                        .rounded_sm()
                        .when(band_fills, |g| g.flex_1().min_h_0())
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(pal.dim)
                                .child(SharedString::from(t.replace('&', ""))),
                        )
                        .children(drawn)
                        .into_any_element(),
                ),
                None => out.extend(drawn),
            }
        }
        out
    }

    /// The top dialog, centred over a backdrop that covers the whole window,
    /// so the pointer cannot reach anything under it. A popup (#1607) is
    /// placed at its [`dialog::Popup`] instead, over an undimmed backdrop.
    pub(crate) fn dialog_overlay(&self, pal: Pal, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.active_dialogs()?.top()?;
        // The scrolls belong to this opening of this dialog (#1609: every
        // plugin dialog shares one id, and a reopened dialog starts afresh).
        self.dialog_lists
            .borrow_mut()
            .retain(|(epoch, _), _| *epoch == d.epoch);
        self.dialog_grids
            .borrow_mut()
            .retain(|(epoch, _), _| *epoch == d.epoch);
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
        let controls = self.dialog_body(d, pal, cx);
        // Project's button places (#1609): Help bottom left, the rest bottom
        // right; or every button down the right side.
        let buttons_right = d.buttons_right;
        let is_help = |b: &dialog::Button| b.id.eq_ignore_ascii_case("help");
        let help_count = d.buttons.iter().filter(|b| is_help(b)).count();
        let button = |i: usize, b: &dialog::Button| {
            let label = b.label.clone();
            div()
                .id(("dialog-button", i))
                .relative()
                .min_w(px(72.))
                .when(buttons_right, |el| el.w(px(96.)))
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
        };
        // A dialog drawn as a popup (#1607): menu rows, the value list, then
        // OK / Cancel, under the heading it opened from.
        if let Some(p) = d.popup {
            let rows = d
                .buttons
                .iter()
                .enumerate()
                .filter(|(_, b)| b.role == dialog::ButtonRole::Apply)
                .map(|(i, b)| {
                    let label = b.label.clone();
                    let color = if b.enabled { pal.fg } else { pal.dim };
                    v_flex()
                        .child(div().when(b.rule, |r| r.h(px(1.)).mx_2().my(px(3.)).bg(pal.border)))
                        .child(
                            h_flex()
                                .id(("dialog-button", i))
                                .relative()
                                .h(px(crate::project::MENU_ROW_H))
                                .px_2()
                                .gap_2()
                                .items_center()
                                .text_size(px(12.))
                                .text_color(color)
                                .child(probe(&self.probes, format!("dialog-button:{}", b.label)))
                                .child(div().w(px(16.)).flex_none().when(!b.icon.is_empty(), |d| {
                                    d.child(icon_svg(b.icon, 16., color))
                                }))
                                .child(div().flex_1().child(SharedString::from(b.label.clone())))
                                .when(b.submenu, |r| r.child("\u{25b8}"))
                                .when(b.enabled, |r| {
                                    r.cursor_pointer().hover(|r| r.bg(pal.hover)).on_click(
                                        cx.listener(move |this, _, window, cx| {
                                            this.dialog_button_click(&label, window, cx)
                                        }),
                                    )
                                }),
                        )
                })
                .collect::<Vec<_>>();
            let foot = d
                .buttons
                .iter()
                .enumerate()
                .filter(|(_, b)| b.role != dialog::ButtonRole::Apply)
                .map(|(i, b)| button(i, b))
                .collect::<Vec<_>>();
            return Some(
                deferred(
                    div()
                        .id("dialog-backdrop")
                        .absolute()
                        .inset_0()
                        .occlude()
                        // A press anywhere else closes it as Cancel.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.dialog_button_click("Cancel", window, cx)
                            }),
                        )
                        .child(
                            v_flex()
                                .id("dialog")
                                .absolute()
                                .left(px(p.x))
                                .top(px(p.y))
                                .w(px(p.width))
                                .py_1()
                                .bg(pal.panel)
                                .border_1()
                                .border_color(pal.border)
                                .shadow_lg()
                                .occlude()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(probe(&self.probes, "dialog"))
                                .children(rows)
                                .child(div().h(px(1.)).mx_2().my(px(3.)).bg(pal.border))
                                .children(controls.into_iter().map(|c| div().px_2().child(c)))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .px_2()
                                        .pt_2()
                                        .pb_1()
                                        .justify_end()
                                        .children(foot),
                                ),
                        ),
                )
                .with_priority(2)
                .into_any_element(),
            );
        }
        let buttons: Vec<_> = d
            .buttons
            .iter()
            .enumerate()
            .map(|(i, b)| button(i, b))
            .collect();
        // A modeless dialog (#1602) has no backdrop: the pointer reaches the
        // document, and a press in the panel gives the dialog the keyboard.
        let modeless = d.modeless;
        // A fixed size (#1609) shared by every page.
        let sized = d.size.is_some();
        Some(
            deferred(
                div()
                    .id("dialog-backdrop")
                    .absolute()
                    .inset_0()
                    .when(!modeless, |b| {
                        b.occlude().bg(Hsla {
                            a: 0.25,
                            ..hsla_u(0x000000)
                        })
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        v_flex()
                            .id("dialog")
                            .relative()
                            .when(!sized, |p| p.min_w(px(320.)).max_w(px(560.)))
                            .when_some(d.size, |p, (w, h)| p.w(px(w)).h(px(h)).overflow_hidden())
                            .p_4()
                            .gap_3()
                            .bg(pal.panel)
                            .border_1()
                            .border_color(if modeless && !d.active {
                                pal.dim
                            } else {
                                pal.border
                            })
                            .shadow_lg()
                            .when(modeless, |p| {
                                p.occlude().on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.active_dialogs_mut().activate(true);
                                        cx.notify();
                                    }),
                                )
                            })
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
                            .child(if buttons_right {
                                // The page on the left, the buttons stacked
                                // on the right, as Assign Resources'.
                                h_flex()
                                    .gap_4()
                                    .items_start()
                                    .when(sized, |r| r.flex_1().min_h_0())
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_3()
                                            .when(sized, |v| v.min_h_0().h_full())
                                            .children(controls),
                                    )
                                    .child(v_flex().flex_none().gap_2().children(buttons))
                                    .into_any_element()
                            } else {
                                let mut helps: Vec<AnyElement> = Vec::new();
                                let mut rest: Vec<AnyElement> = Vec::new();
                                for (b, el) in d.buttons.iter().zip(buttons) {
                                    if is_help(b) {
                                        helps.push(el.into_any_element());
                                    } else {
                                        rest.push(el.into_any_element());
                                    }
                                }
                                v_flex()
                                    .gap_3()
                                    .when(sized, |v| v.flex_1().min_h_0())
                                    .child(
                                        v_flex()
                                            .gap_3()
                                            .when(sized, |v| v.flex_1().min_h_0())
                                            .children(controls),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .when(help_count > 0, |r| {
                                                r.justify_between()
                                                    .child(h_flex().gap_2().children(helps))
                                            })
                                            .when(help_count == 0, |r| r.justify_end())
                                            .child(h_flex().gap_2().children(rest)),
                                    )
                                    .into_any_element()
                            }),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

/// A press in a date combo's calendar.
#[derive(Clone, Copy)]
enum CalendarAct {
    /// The ▾ button: open the calendar, or close it.
    Toggle,
    /// Previous or next month.
    Month(i32),
    /// A day of the shown month, which is picked.
    Day(u32),
}

/// How far in from the left of a field's `dialog-field` probe its text
/// starts: the box's `px_1` padding. The probe is absolutely positioned inside
/// the box's border, so the one-pixel border is already outside it.
const FIELD_INSET: f32 = 4.;

/// The character gap of a multiline's `text` nearest to `x` pixels along
/// the line `y` pixels down from its first line (lines are 16 px, #1609):
/// the gaps of the lines above plus [`char_at_x`] on that line.
fn char_at_line_x(text: &str, y: f32, x: f32, width: impl Fn(&str) -> f32) -> usize {
    let lines: Vec<&str> = text.split('\n').collect();
    let line = ((y / 16.).floor().max(0.) as usize).min(lines.len().saturating_sub(1));
    let before: usize = lines[..line].iter().map(|l| l.chars().count() + 1).sum();
    before + char_at_x(lines[line], x, width)
}

/// The character gap nearest to `x` pixels along `text` (a gap is between
/// characters, i.e. grapheme clusters, never inside one), whose prefixes
/// measure as `width` says.
fn char_at_x(text: &str, x: f32, width: impl Fn(&str) -> f32) -> usize {
    let mut at = 0;
    let mut prev = 0.0;
    for (i, g) in text.grapheme_indices(true) {
        let w = width(&text[..i + g.len()]);
        if x < (prev + w) / 2.0 {
            return at;
        }
        prev = w;
        at += g.chars().count();
    }
    at
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
    use crate::text_input::ImeState;
    use core::prelude::v1::test;

    /// #988 r1: a list's choice that moves is scrolled into view once; a
    /// frame with the same choice asks for no scroll (a wheel scroll stays),
    /// and the handle is the same one frame to frame.
    /// #1609: a click in a multiline lands on the line under the pointer,
    /// the gaps of the lines above counted, however far the box scrolled
    /// (the distance is measured from the first line, which scrolls too).
    #[test]
    fn a_multiline_click_lands_on_its_line() {
        let width = |s: &str| s.chars().count() as f32 * 7.;
        let text = "one\ntwo\nthree";
        assert_eq!(char_at_line_x(text, 0., 0., width), 0);
        assert_eq!(
            char_at_line_x(text, 20., 0., width),
            4,
            "line 1 starts after 'one' and its break"
        );
        assert_eq!(
            char_at_line_x(text, 20., 100., width),
            7,
            "past the end of 'two'"
        );
        assert_eq!(char_at_line_x(text, 40., 10., width), 9, "'th|ree'");
        assert_eq!(
            char_at_line_x(text, 400., 0., width),
            8,
            "below the last line: its start"
        );
        assert_eq!(char_at_line_x("", 50., 50., width), 0);
    }

    /// #1609 r5: a caret that moves along the line it is on (typing after a
    /// wheel scroll took the line out of view) brings the line back; a frame
    /// with the same caret keeps the wheel's offset, as a grid's unchanged
    /// row does. r6: so does an edit that leaves the caret in place (Delete),
    /// through the dialog's edit count in the mark.
    #[test]
    fn a_multiline_edit_on_the_same_line_brings_it_back_into_view() {
        let mut grids = GridScrolls::default();
        let key = (1, "memo");
        // Line 9 of 16 px in a 64 px box: the least scroll that shows it.
        let h = grid_scroll(&mut grids, key, Some((9, 40)), 16., Some(64.));
        assert_eq!(f32::from(h.offset().y), -96.);
        // The wheel scrolls back to the top; the same caret leaves it there.
        h.set_offset(point(px(0.), px(0.)));
        let h = grid_scroll(&mut grids, key, Some((9, 40)), 16., Some(64.));
        assert_eq!(f32::from(h.offset().y), 0.);
        // A character typed on line 9 moves the caret: the line comes back.
        let h = grid_scroll(&mut grids, key, Some((9, 41)), 16., Some(64.));
        assert_eq!(f32::from(h.offset().y), -96.);
        // A Delete keeps the caret at 41 but counts as an edit: back again.
        h.set_offset(point(px(0.), px(0.)));
        let h = grid_scroll(&mut grids, key, Some((9, (1 << 32) | 41)), 16., Some(64.));
        assert_eq!(f32::from(h.offset().y), -96.);
        // Nothing painted yet: the handle has nothing to scroll, so the
        // read-back reports no line until a frame has measured the box.
        assert_eq!(first_visible(&grids[&key]), None);
    }

    #[test]
    fn a_list_scrolls_a_changed_choice_into_view_once() {
        let mut lists = ListScrolls::default();
        let key = (1, "refs");
        let pending = |h: &UniformListScrollHandle| {
            h.0.borrow_mut()
                .deferred_scroll_to_item
                .take()
                .map(|d| d.item_index)
        };
        let h = list_scroll(&mut lists, key, None);
        assert_eq!(pending(&h), None, "nothing chosen: no scroll");
        let h = list_scroll(&mut lists, key, Some(40));
        assert_eq!(pending(&h), Some(40));
        let again = list_scroll(&mut lists, key, Some(40));
        assert!(std::rc::Rc::ptr_eq(&h.0, &again.0));
        assert_eq!(pending(&again), None, "the same choice: left alone");
        let h = list_scroll(&mut lists, key, Some(0));
        assert_eq!(pending(&h), Some(0));
        let other = list_scroll(&mut lists, (2, "names"), Some(3));
        assert!(!std::rc::Rc::ptr_eq(&h.0, &other.0));
    }

    /// A document tab with a two-field form open: Top (a number) and Note.
    fn tab_with_form() -> DocTab {
        let mut t = tab_from_path(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
        );
        let mut d = Dialog::message(
            crate::dialog::catalog::TEST_FORM,
            "Form",
            String::new(),
            &[],
            DialogOwner::Test,
        );
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

    /// A platform key-down as `Docxy::modal_takes_key` hands it to the
    /// dialog: on macOS the plain typed key when ⌥ made its character.
    fn platform_key(t: &mut DocTab, ev: &KeyDownEvent, macos: bool) -> bool {
        let k = crate::text_input::dialog_stroke(ev, macos);
        dialog_key(t, &k.key, k.key_char.as_deref(), k.modifiers)
    }

    /// A Windows key-down as `Docxy::route_key` takes it: the dialog's key,
    /// then the WM_CHAR debt, decided by whether the dialog was open; then
    /// its WM_CHAR `wm_char`, if one comes at once.
    fn windows_key(t: &mut DocTab, ime: &mut ImeState, ev: &KeyDownEvent, wm_char: Option<&str>) {
        let open = t.dialogs.is_open();
        assert!(platform_key(t, ev, false));
        ime.typed_key(ev, std::time::Instant::now(), open);
        if let Some(text) = wm_char {
            windows_char(t, ime, text);
        }
    }

    /// A WM_CHAR as `Docxy::windows_char` takes it: the keys it types go to
    /// the dialog; with none open, the count of keys for the surface below.
    fn windows_char(t: &mut DocTab, ime: &mut ImeState, text: &str) -> usize {
        let open = t.dialogs.is_open();
        let keys = ime.char_message(text, std::time::Instant::now(), open);
        for k in &keys {
            assert!(platform_key(t, k, false));
        }
        if open { 0 } else { keys.len() }
    }

    fn key_down(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                modifiers,
                key: key.into(),
                key_char: key_char.map(Into::into),
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    /// The issue's keys on macOS (#1162): ⌥G, ⌥2 and ABC-India's ⌥` type
    /// their characters into a dialog field, as in the document. ⌥ keys
    /// with no character of their own, and ⌘⌥ or ⌃⌥ chords, still edit
    /// nothing.
    #[test]
    fn option_characters_type_into_a_dialog_field_on_macos() {
        let mut t = tab_with_form();
        assert!(key(&mut t, "tab", None));
        assert!(key(&mut t, "tab", None));
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let alt_shift = Modifiers { shift: true, ..alt };
        let plain = Modifiers::default();
        for ev in [
            key_down("`", Some("`"), plain),
            key_down("space", Some(" "), plain),
            key_down("x", Some("x"), plain),
            key_down("g", Some("©"), alt),
            key_down("2", Some("™"), alt),
            key_down("`", Some("`"), alt),
            key_down("y", Some("Á"), alt_shift),
            key_down("y", Some("y"), plain),
        ] {
            assert!(platform_key(&mut t, &ev, true));
        }
        assert_eq!(value(&t, "note"), Value::Text("` x©™`Áy".into()));
        let swallowed = [
            key_down("backspace", Some("\u{8}"), alt),
            key_down(
                "left",
                None,
                Modifiers {
                    function: true,
                    ..alt
                },
            ),
            key_down(
                "g",
                Some("©"),
                Modifiers {
                    platform: true,
                    ..alt
                },
            ),
            key_down(
                "g",
                Some("©"),
                Modifiers {
                    control: true,
                    ..alt
                },
            ),
        ];
        for ev in &swallowed {
            assert!(platform_key(&mut t, ev, true));
        }
        assert_eq!(value(&t, "note"), Value::Text("` x©™`Áy".into()));
        // Off macOS an Alt key-down is no text.
        assert!(platform_key(&mut t, &key_down("g", Some("©"), alt), false));
        assert_eq!(value(&t, "note"), Value::Text("` x©™`Áy".into()));
    }

    /// Windows AltGr (#1162): the character's WM_CHAR types it into a
    /// dialog field once; an AltGr dead key, which sends no WM_CHAR, types
    /// nothing and its accent joins the next letter ("ñ", not "~ñ"); plain
    /// Alt (WM_SYSCHAR, no WM_CHAR) edits nothing.
    #[test]
    fn altgr_characters_type_into_a_dialog_field_on_windows() {
        let mut t = tab_with_form();
        assert!(key(&mut t, "tab", None));
        assert!(key(&mut t, "tab", None));
        let mut ime = ImeState::default();
        let ctrl_alt = Modifiers {
            control: true,
            alt: true,
            ..Modifiers::default()
        };
        let altgr = |key: &str, ch: &str| KeyDownEvent {
            prefer_character_input: true,
            ..key_down(key, Some(ch), ctrl_alt)
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        windows_key(&mut t, &mut ime, &altgr("q", "@"), Some("@"));
        windows_key(&mut t, &mut ime, &altgr("e", "€"), Some("€"));
        windows_key(&mut t, &mut ime, &altgr("4", "~"), None);
        assert_eq!(value(&t, "note"), Value::Text("@€".into()));
        let n = key_down("n", Some("ñ"), Modifiers::default());
        windows_key(&mut t, &mut ime, &n, Some("ñ"));
        windows_key(&mut t, &mut ime, &key_down("g", Some("g"), alt), None);
        assert_eq!(value(&t, "note"), Value::Text("@€ñ".into()));
        // AltGr+Q, then Enter closes the dialog before the "@" WM_CHAR comes
        // (gpui drained input alone): the "@" reaches nothing below.
        windows_key(&mut t, &mut ime, &altgr("q", "@"), None);
        windows_key(
            &mut t,
            &mut ime,
            &key_down("enter", None, Modifiers::default()),
            None,
        );
        assert!(!t.dialogs.is_open());
        assert_eq!(windows_char(&mut t, &mut ime, "@"), 0);
        // The tilde dead key's debt alone is left, to lapse.
        assert_eq!(ime.owed(), 1);
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

    /// A click never lands the caret inside a grapheme cluster: with
    /// "e\u{301}" 20px wide at 10px per character, a gap inside it (1) is
    /// never returned; x falls to the cluster's near or far side (#1035).
    #[test]
    fn a_click_never_lands_inside_a_cluster() {
        // Every character 10px wide.
        let w = |s: &str| s.chars().count() as f32 * 10.0;
        assert_eq!(char_at_x("e\u{301}x", 9.0, w), 0);
        assert_eq!(char_at_x("e\u{301}x", 11.0, w), 2);
        assert_eq!(char_at_x("e\u{301}x", 24.0, w), 2);
        assert_eq!(char_at_x("e\u{301}x", 26.0, w), 3);
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
