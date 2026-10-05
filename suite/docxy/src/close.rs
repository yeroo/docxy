//! Single-tab close always asks about dirty work: removing a tab also removes
//! it from hot-exit recovery. `ask_on_close` only governs closing the window.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseAnswer {
    Save,
    Discard,
    Cancel,
}

#[derive(Debug, PartialEq, Eq)]
enum CloseStep {
    /// Clean: remove it.
    Remove,
    /// Dirty, answered Don't Save: remove it, keeping a draft if it qualifies.
    Discard,
    Save,
    Keep,
    Refuse(String),
}

impl CloseStep {
    /// Whether the tab goes away without saving. Neither kind of removal
    /// activates the tab first, so the previous active tab is kept.
    fn removes(&self) -> bool {
        matches!(self, Self::Remove | Self::Discard)
    }
}

/// Whether closing a tab keeps its last AutoRecover copy as a draft (#613):
/// a workbook `discarded` with Don't Save, with AutoRecover on (`minutes`)
/// and "Keep the last AutoRecovered version" on (`keep`), and a hot-exit
/// write while it was unsaved (`last_hot`). Window close never comes here:
/// hot exit keeps those tabs as tabs.
fn should_keep_draft(
    kind: Kind,
    minutes: u32,
    keep: bool,
    discarded: bool,
    last_hot: Option<&std::path::Path>,
) -> bool {
    discarded && kind == Kind::Xlsx && minutes > 0 && keep && last_hot.is_some()
}

/// Keep `tab`'s draft under `root` when [`should_keep_draft`] says so:
/// `None` when it does not, else where the draft went or why it was not kept.
fn keep_closed_draft(
    root: &std::path::Path,
    tab: &DocTab,
    step: &CloseStep,
    minutes: u32,
    keep: bool,
    now: std::time::SystemTime,
) -> Option<Result<PathBuf, String>> {
    let last_hot = tab.last_hot.borrow();
    let sidecar = last_hot.as_deref();
    if !should_keep_draft(
        tab.kind,
        minutes,
        keep,
        *step == CloseStep::Discard,
        sidecar,
    ) {
        return None;
    }
    sidecar.map(|s| recover::keep_draft(root, &tab.title, s, now))
}

/// Where a draft that could not be kept is reported. The tab is already
/// gone, so the status line of whichever tab is left; with none left, a
/// warning, except under the harness (no native modal there), whose
/// `close-tab` reply carries it.
#[derive(Debug, PartialEq, Eq)]
enum DraftErrorTo {
    Status,
    Dialog,
    Reply,
}

fn draft_error_to(tabs_left: bool, harness: bool) -> DraftErrorTo {
    match (tabs_left, harness) {
        (true, _) => DraftErrorTo::Status,
        (false, false) => DraftErrorTo::Dialog,
        (false, true) => DraftErrorTo::Reply,
    }
}

fn commit_pending_for_close(tab: &mut DocTab) -> Result<(), String> {
    if !commit_project_cell(tab) {
        // Keep the exact error: Project clears it on correction by comparing
        // the status with the cell editor's last_error.
        return Err(tab.status.to_string());
    }
    // An unfinished sheet formula (`=SUM(A1`) refuses too, as an invalid
    // Project buffer does: closing would drop what was typed.
    commit_changed_cell(tab)?;
    exit_hf_tab(tab);
    Ok(())
}

/// Window close and harness `quit`: fold every tab's pending edit into what
/// hot-exit persists. Best-effort, never refuses: an invalid Project buffer,
/// like a sheet cell editor holding an entry the sheet refuses (an unfinished
/// formula such as `=SUM(A1`), stays uncommitted and the last committed model
/// is persisted. Header/footer
/// is flushed rather than exited, so a cancelled window close keeps the user in
/// header/footer mode.
pub(crate) fn commit_pending_for_exit(tabs: &mut [DocTab]) {
    for tab in tabs {
        flush_level_pass(tab);
        let _ = commit_project_cell(tab);
        let _ = commit_changed_cell(tab);
        flush_hf_tab(tab);
    }
}

/// Commit a sheet's open cell editor unless it was seeded from a cell and left
/// unchanged. `commit_edit` also skips that case, but taking the buffer here
/// would close the editor; cancelled close or Save As must leave it open.
/// `Err` (the reason, also put in the tab's status) when the sheet refused the
/// entry — an unfinished formula, or one over the cell limit — and the editor
/// stays open with the text.
fn commit_changed_cell(tab: &mut DocTab) -> Result<(), String> {
    if let Surface::Sheet(v) = &mut tab.surface
        && v.editing.is_some()
        && !v.edit_untouched()
    {
        let changed = v.commit_edit();
        if v.editing.is_some() {
            let message = v
                .entry_error
                .take()
                .unwrap_or_else(|| "the cell entry could not be committed".into());
            tab.status = message.clone().into();
            return Err(message);
        }
        tab.dirty |= changed;
        if changed {
            v.anchor = v.sel;
            v.clear_areas();
        }
    }
    Ok(())
}

/// Apply the cell-editor part of Save before choosing a target and writing.
/// Keep this path shared with close so Save cannot reparse an untouched editor.
/// `Err` when the editor holds an entry the sheet refuses: Save must not write
/// (or mark clean) a workbook without what is being typed.
pub(crate) fn prepare_sheet_save(tab: &mut DocTab) -> Result<(), String> {
    commit_changed_cell(tab)
}

fn close_step(
    tab: &mut DocTab,
    ask: impl FnOnce(&DocTab) -> Result<CloseAnswer, String>,
) -> CloseStep {
    if let Err(message) = commit_pending_for_close(tab) {
        return CloseStep::Refuse(message);
    }
    if !tab.dirty {
        return CloseStep::Remove;
    }
    match ask(tab) {
        Ok(CloseAnswer::Save) => CloseStep::Save,
        Ok(CloseAnswer::Discard) => CloseStep::Discard,
        Ok(CloseAnswer::Cancel) => CloseStep::Keep,
        Err(message) => CloseStep::Refuse(message),
    }
}

fn remove_tab(tabs: &mut Vec<DocTab>, active: &mut usize, i: usize) {
    tabs.remove(i);
    if *active >= tabs.len() {
        *active = tabs.len().saturating_sub(1);
    } else if i < *active {
        *active -= 1;
    }
}

impl Docxy {
    pub(super) fn backstage_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.backstage = false;
        self.close_tab(self.active, window, cx);
    }

    pub(super) fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_with(i, None, window, cx);
    }

    /// Close tab `i`, asking about unsaved work (or taking `answer` under the
    /// harness). Returns why a Don't Save draft was not kept, if it was not
    /// (#613): the harness reports it, since with no tab left there is no
    /// status line to carry it.
    pub(super) fn close_tab_with(
        &mut self,
        i: usize,
        answer: Option<CloseAnswer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if i >= self.tabs.len() {
            return None;
        }
        self.flush_project_passes(cx);
        self.project_prompt_cancel();
        let previous_active = self.active;
        let harness = self.harness.is_some();
        let step = close_step(&mut self.tabs[i], |tab| {
            if harness {
                // A native modal dialog blocks the harness control pump.
                return answer.ok_or_else(|| {
                    "Unsaved changes: close refused; a harness close needs save, discard or cancel"
                        .into()
                });
            }
            let result = rfd::MessageDialog::new()
                .set_title("docxy")
                .set_description(format!("Save changes to {} before closing?", tab.title))
                .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                    "Save".into(),
                    "Don't Save".into(),
                    "Cancel".into(),
                ))
                .show();
            Ok(match result {
                rfd::MessageDialogResult::Custom(label) if label == "Save" => CloseAnswer::Save,
                rfd::MessageDialogResult::Custom(label) if label == "Don't Save" => {
                    CloseAnswer::Discard
                }
                _ => CloseAnswer::Cancel,
            })
        });
        if !step.removes() && self.active != i {
            self.active = i;
            self.drop_grid_state();
        }
        // Kept before removal: the sidecar is rewritten for whichever tab
        // takes this index at the persist below.
        let mut draft_error = None;
        if let Some(kept) = keep_closed_draft(
            &config_root(),
            &self.tabs[i],
            &step,
            self.autorecover_minutes,
            self.keep_drafts,
            std::time::SystemTime::now(),
        ) {
            match kept {
                Ok(_) => {
                    self.refresh_drafts();
                }
                // Never blocks the close: the user chose to discard.
                Err(e) => draft_error = Some(e),
            }
        }
        let remove = match step {
            CloseStep::Remove | CloseStep::Discard => true,
            CloseStep::Keep => false,
            CloseStep::Refuse(message) => {
                self.tabs[i].status = message.into();
                false
            }
            CloseStep::Save => {
                self.save_active(window, cx);
                !self.tabs[i].dirty
            }
        };
        if remove {
            // Saving temporarily activates the target. A successful close must
            // preserve the same previous tab as a clean close or Don't Save.
            self.active = previous_active;
            remove_tab(&mut self.tabs, &mut self.active, i);
            // The Info page's result is keyed by tab index (#627).
            self.bs_info_status = None;
            self.drop_grid_state();
        }
        if let Some(e) = &draft_error {
            match draft_error_to(!self.tabs.is_empty(), harness) {
                DraftErrorTo::Status => self.set_status(e.clone()),
                DraftErrorTo::Dialog => {
                    rfd::MessageDialog::new()
                        .set_title("docxy")
                        .set_level(rfd::MessageLevel::Warning)
                        .set_description(e)
                        .set_buttons(rfd::MessageButtons::Ok)
                        .show();
                }
                DraftErrorTo::Reply => {}
            }
        }
        self.persist();
        self.refocus(window, cx);
        draft_error
    }
}

#[cfg(test)]
mod tests;
