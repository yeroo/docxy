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
    Remove,
    Save,
    Keep,
    Refuse(String),
}

pub(super) fn flush_hf_tab(tab: &mut DocTab) {
    let Some(hf) = tab.hf_edit.as_ref() else {
        return;
    };
    let inner = docxcore::serialize::blocks_to_xml(&hf.editor.doc.body);
    let tag = if hf.is_header { "w:hdr" } else { "w:ftr" };
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <{tag} xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:m=\"{M_NS}\">{inner}</{tag}>"
    );
    if let Some(pkg) = tab.pkg.as_mut() {
        pkg.set_part(&hf.part_name, xml.into_bytes());
    }
    tab.dirty = true;
}

fn commit_pending_for_close(tab: &mut DocTab) -> Result<(), String> {
    if !commit_project_cell(tab) {
        // Keep the exact error: Project clears it on correction by comparing
        // the status with the cell editor's last_error.
        return Err(tab.status.to_string());
    }
    if let Surface::Sheet(v) = &mut tab.surface {
        tab.dirty |= v.commit_edit();
    }
    flush_hf_tab(tab);
    if tab.hf_edit.take().is_some() {
        tab.status = "Closed header/footer".into();
    }
    Ok(())
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
        Ok(CloseAnswer::Discard) => CloseStep::Remove,
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

    pub(super) fn close_tab_with(
        &mut self,
        i: usize,
        answer: Option<CloseAnswer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if i >= self.tabs.len() {
            return;
        }
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
        if step != CloseStep::Remove && self.active != i {
            self.active = i;
            self.drop_grid_state();
        }
        let remove = match step {
            CloseStep::Remove => true,
            CloseStep::Keep => false,
            CloseStep::Refuse(message) => {
                self.tabs[i].status = message.into();
                false
            }
            CloseStep::Save => {
                self.save_for_close(window, cx);
                !self.tabs[i].dirty
            }
        };
        if remove {
            // Saving temporarily activates the target. A successful close must
            // preserve the same previous tab as a clean close or Don't Save.
            self.active = previous_active;
            remove_tab(&mut self.tabs, &mut self.active, i);
            self.drop_grid_state();
        }
        self.persist();
        self.refocus(window, cx);
    }

    fn save_for_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = &self.tabs[self.active];
        // Normal document Save currently chooses cwd/title for an untitled
        // document. Close must ask for a destination instead of overwriting it.
        if matches!(tab.surface, Surface::Doc(_)) && tab.path.is_none() {
            if self.harness.is_some() {
                self.tabs[self.active].status =
                    "this document has never been saved, and a harness instance cannot open the Save As dialog".into();
                return;
            }
            if !self.pick_doc_save_target() {
                self.tabs[self.active].status = "save cancelled".into();
                return;
            }
        }
        self.save_active(window, cx);
    }
}

#[cfg(test)]
mod tests;
