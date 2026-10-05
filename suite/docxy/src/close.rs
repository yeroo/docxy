//! Single-tab close always asks about dirty work: removing a tab also removes
//! it from hot-exit recovery. `ask_on_close` only governs closing the window.
//!
//! The question is an in-app dialog on the tab's own [`DialogStack`] (#629),
//! never a native box: Word's "Save your changes to this file?" with a File
//! name, a location and More options... for a document, and the plain "Save
//! changes to … before closing?" for a workbook or a Project. Its presses
//! come through [`Docxy::close_prompt_click`].
use super::*;
use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use std::path::Path;

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
    /// Dirty, with no answer yet: open the close prompt.
    Ask,
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

/// What closing `tab` does: `answer` is how a dirty tab was answered, or
/// `None` when it has not been asked yet.
fn close_step(tab: &mut DocTab, answer: impl FnOnce(&DocTab) -> Option<CloseAnswer>) -> CloseStep {
    if let Err(message) = commit_pending_for_close(tab) {
        return CloseStep::Refuse(message);
    }
    if !tab.dirty {
        return CloseStep::Remove;
    }
    match answer(tab) {
        Some(CloseAnswer::Save) => CloseStep::Save,
        Some(CloseAnswer::Discard) => CloseStep::Discard,
        Some(CloseAnswer::Cancel) => CloseStep::Keep,
        None => CloseStep::Ask,
    }
}

/// What a tab with a dialog open says when it is asked to close: the
/// dialog is staged work of its own, so it is closed first.
pub(crate) const CLOSE_DIALOG_FIRST: &str = "Close the open dialog first";

/// What More options... says under the harness, which cannot open a native
/// Save As dialog.
const MORE_OPTIONS_HARNESS: &str =
    "a harness instance cannot open the Save As dialog; use the File name and location";

/// Word's close prompt's title.
pub(crate) const SAVE_PROMPT_TITLE: &str = "Save your changes to this file?";

/// Whether the tab's Save writes its own file, as it is (not Save As): what
/// an unchanged File name and location mean in the close prompt.
fn saves_in_place(tab: &DocTab) -> bool {
    tab.path.is_some() && !tab.access.save_needs_dialog() && !tab.import.binary_source
}

/// The close prompt's File name, its fixed extension and its locations for
/// a document tab.
#[derive(Debug, PartialEq, Eq)]
struct PromptName {
    stem: String,
    /// The tab's own save extension (`.md`, `.html`, …) when it saves in
    /// place, else `.docx`: a new name keeps it.
    ext: String,
    locations: Vec<PathBuf>,
}

/// [`PromptName`] for `tab`, offering `known` folders (Documents, Desktop)
/// after its own.
fn prompt_name(tab: &DocTab, known: &[PathBuf]) -> PromptName {
    let in_place = saves_in_place(tab);
    let own_ext = tab
        .path
        .as_deref()
        .filter(|p| in_place && doc_target_allowed(p))
        .and_then(|p| p.extension())
        .map(|e| format!(".{}", e.to_string_lossy()));
    let ext = own_ext.unwrap_or_else(|| ".docx".into());
    let name = match tab.path.as_deref().filter(|_| in_place) {
        Some(path) => file_name(path),
        None => doc_save_as_name(tab),
    };
    let stem = name
        .strip_suffix(&ext)
        .or_else(|| {
            let lower = name.to_ascii_lowercase();
            lower
                .ends_with(&ext.to_ascii_lowercase())
                .then(|| &name[..name.len() - ext.len()])
        })
        .unwrap_or(&name)
        .to_string();
    // The tab's own folder first: its file's, or an imported document's
    // original's.
    let own_dir = doc_import::save_dir(tab)
        .map(Path::to_path_buf)
        .or_else(|| {
            tab.path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        })
        .map(|d| {
            if d.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                d
            }
        });
    let mut locations: Vec<PathBuf> = Vec::new();
    for dir in own_dir.into_iter().chain(known.iter().cloned()) {
        if !locations.iter().any(|l| l == &dir) {
            locations.push(dir);
        }
    }
    if locations.is_empty() {
        locations.push(PathBuf::from("."));
    }
    PromptName {
        stem,
        ext,
        locations,
    }
}

/// The folders every close prompt offers after the tab's own. A harness
/// instance offers its sandbox instead, so a script never writes into the
/// user's own folders.
fn known_locations(harness: bool) -> Vec<PathBuf> {
    if harness {
        return vec![config_root()];
    }
    let known: Vec<PathBuf> = [dirs::document_dir(), dirs::desktop_dir()]
        .into_iter()
        .flatten()
        .collect();
    if known.is_empty() {
        dirs::home_dir().into_iter().collect()
    } else {
        known
    }
}

/// Where the close prompt's Save writes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PromptSave {
    /// The tab's own file, as Save would.
    InPlace,
    /// A new file.
    To(PathBuf),
}

/// The characters Word refuses in a file name.
const BAD_NAME_CHARS: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// Where Save goes for the File name `stem` in `location`, with the fixed
/// extension `ext`: the tab's own file in place when that is what it names
/// and the tab saves in place (`in_place`), else that new file. An existing
/// file that is not the tab's own (`own`) is refused, as are an empty name
/// and one with characters a file name cannot hold.
fn prompt_target(
    stem: &str,
    ext: &str,
    location: &Path,
    own: Option<&Path>,
    in_place: bool,
) -> Result<PromptSave, String> {
    let mut stem = stem.trim();
    // A typed extension is the fixed one: `report.docx` is `report`.
    if stem.len() > ext.len()
        && stem
            .to_ascii_lowercase()
            .ends_with(&ext.to_ascii_lowercase())
    {
        stem = stem[..stem.len() - ext.len()].trim_end();
    }
    if stem.is_empty() {
        return Err("Type a file name to save the document".into());
    }
    if stem.contains(BAD_NAME_CHARS) || stem.chars().any(char::is_control) {
        return Err(
            "A file name can't contain any of these characters: \\ / : * ? \" < > |".into(),
        );
    }
    let target = location.join(format!("{stem}{ext}"));
    let own_file = own.is_some_and(|own| writes_own_file(Some(own), Some(&target)));
    if own_file && in_place {
        return Ok(PromptSave::InPlace);
    }
    if !own_file && target.exists() {
        return Err(format!(
            "{} already exists; choose another name, or use More options... to replace it",
            file_name(&target)
        ));
    }
    Ok(PromptSave::To(target))
}

/// Word's close prompt for a document tab.
fn doc_prompt(name: &PromptName, quit: bool) -> Dialog {
    let mut d = Dialog::message(
        "save-on-close",
        SAVE_PROMPT_TITLE,
        String::new(),
        &[],
        DialogOwner::SaveOnClose { quit },
    );
    d.text = None;
    let mut location = Control::new(
        "location",
        "Choose a Location:",
        ControlKind::Dropdown,
        Value::Choice(Some(0)),
    );
    location.items = name
        .locations
        .iter()
        .map(|l| l.display().to_string())
        .collect();
    d.controls = vec![
        Control::new(
            "file-name",
            "File name:",
            ControlKind::Text,
            Value::Text(name.stem.clone()),
        ),
        Control::new(
            "extension",
            "",
            ControlKind::Label,
            Value::Text(name.ext.clone()),
        ),
        location,
    ];
    d.buttons = vec![
        Button {
            default: true,
            ..Button::new("Save", ButtonRole::Accept)
        },
        Button::new("Don't Save", ButtonRole::Accept),
        Button::new("Cancel", ButtonRole::Cancel),
        Button::new("More options...", ButtonRole::Apply),
    ];
    d.focus = Some(0);
    d.mark_opened();
    d
}

/// The close prompt for a workbook or a Project tab titled `title`.
fn message_prompt(title: &str, quit: bool) -> Dialog {
    Dialog::message(
        "save-on-close",
        "docxy",
        format!("Save changes to {title} before closing?"),
        &[
            ("Save", ButtonRole::Accept),
            ("Don't Save", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::SaveOnClose { quit },
    )
}

/// The close prompt for `tab`, offering `known` folders after its own.
fn close_prompt(tab: &DocTab, quit: bool, known: &[PathBuf]) -> Dialog {
    match &tab.surface {
        Surface::Doc(_) => doc_prompt(&prompt_name(tab, known), quit),
        _ => message_prompt(&tab.title, quit),
    }
}

/// Where the open document close prompt `d` saves `tab`.
fn prompt_dialog_target(tab: &DocTab, d: &Dialog) -> Result<PromptSave, String> {
    let stem = crate::page_setup::text_of(d, "file-name");
    let ext = crate::page_setup::text_of(d, "extension");
    let location = crate::page_setup::text_of(d, "location");
    prompt_target(
        &stem,
        &ext,
        Path::new(&location),
        tab.path.as_deref(),
        saves_in_place(tab),
    )
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

    /// Close tab `i`. A dirty tab with no `answer` (the harness's
    /// `close-tab` may give one) opens the close prompt on it, which answers
    /// later through [`Self::close_prompt_click`]. Returns why a Don't Save
    /// draft was not kept, if it was not (#613): the harness reports it,
    /// since with no tab left there is no status line to carry it.
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
        if self.tabs[i].dialogs.is_open() {
            self.refuse_close_under_dialog(i, window, cx);
            return None;
        }
        self.flush_project_passes(cx);
        self.project_prompt_cancel();
        let previous_active = self.active;
        let harness = self.harness.is_some();
        let step = close_step(&mut self.tabs[i], |_| answer);
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
            CloseStep::Ask => {
                let prompt = close_prompt(&self.tabs[i], false, &known_locations(harness));
                self.tabs[i].dialogs.push(prompt);
                false
            }
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

    /// A tab with a dialog open does not close: it comes to the front with
    /// its dialog, and says so.
    fn refuse_close_under_dialog(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.active != i {
            self.active = i;
            self.drop_grid_state();
        }
        self.tabs[i].status = CLOSE_DIALOG_FIRST.into();
        self.refocus(window, cx);
    }

    /// A press of `button` on the active tab's close prompt; `None` when
    /// the dialog on top is not one. Save saves (in place, or to the File
    /// name in the location) and then closes; a save that fails leaves the
    /// prompt open with the reason. Don't Save closes without saving,
    /// Cancel keeps the tab, and More options... asks for a file with Save
    /// As and saves there.
    pub(crate) fn close_prompt_click(
        &mut self,
        button: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let i = self.active;
        let top = self.tabs.get(i)?.dialogs.top()?;
        let DialogOwner::SaveOnClose { quit } = top.owner else {
            return None;
        };
        let Some(pressed) = top
            .buttons
            .iter()
            .find(|b| b.label.eq_ignore_ascii_case(button) && b.enabled)
            .map(|b| b.label.clone())
        else {
            let labels: Vec<&str> = top.buttons.iter().map(|b| b.label.as_str()).collect();
            return Some(Err(format!(
                "no button '{button}'; buttons: {}",
                labels.join(", ")
            )));
        };
        let done = match pressed.as_str() {
            "Cancel" => {
                self.tabs[i].dialogs.pop();
                self.close_prompt_answered(i, CloseAnswer::Cancel, quit, window, cx);
                Ok(())
            }
            "Don't Save" => {
                self.tabs[i].dialogs.pop();
                self.close_prompt_answered(i, CloseAnswer::Discard, quit, window, cx);
                Ok(())
            }
            "Save" => {
                let target = match &self.tabs[i].surface {
                    Surface::Doc(_) => prompt_dialog_target(&self.tabs[i], top),
                    _ => Ok(PromptSave::InPlace),
                };
                target.and_then(|t| self.close_prompt_save(i, t, quit, window, cx))
            }
            _ if self.harness.is_some() => Err(MORE_OPTIONS_HARNESS.into()),
            _ => match self.pick_doc_save_target() {
                Some(path) => self.close_prompt_save(i, PromptSave::To(path), quit, window, cx),
                // Cancelled: back to the prompt.
                None => Ok(()),
            },
        };
        cx.notify();
        Some(done)
    }

    /// The close prompt's Save on tab `i` (the active one): save to
    /// `target`, then close as answered. A save that does not leave the tab
    /// clean keeps the prompt open and says why.
    fn close_prompt_save(
        &mut self,
        i: usize,
        target: PromptSave,
        quit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match target {
            PromptSave::InPlace => self.save_active(window, cx),
            PromptSave::To(path) => {
                self.save_doc_to(Some(path), window, cx);
            }
        }
        let tab = &mut self.tabs[i];
        if tab.dirty {
            return Err(tab.status.to_string());
        }
        tab.dialogs.pop();
        self.close_prompt_answered(i, CloseAnswer::Save, quit, window, cx);
        Ok(())
    }

    /// Tab `i`'s close prompt was answered (and closed). A tab's own close
    /// closes it as answered; a Save has already saved it, so it closes
    /// clean. While quitting (#630), Cancel stops the quit, and the other
    /// answers go on to the next unsaved tab.
    fn close_prompt_answered(
        &mut self,
        i: usize,
        answer: CloseAnswer,
        quit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (answer, quit) {
            (CloseAnswer::Cancel, false) => {}
            (CloseAnswer::Save | CloseAnswer::Discard, false) => {
                self.close_tab_with(i, Some(answer), window, cx);
            }
            (CloseAnswer::Cancel, true) => self.quit_cancelled(),
            (CloseAnswer::Save, true) => self.next_quit_prompt(window, cx),
            (CloseAnswer::Discard, true) => {
                self.quit_discards.push(i);
                self.next_quit_prompt(window, cx);
            }
        }
    }

    /// The window's close (its X, Alt+F4, the harness `close-window`): fold
    /// every pending edit into the session and persist it. With `ask` and
    /// "Ask before closing the window" on and something unsaved, ask about
    /// each unsaved tab in turn instead (#630): `false` now, and the window
    /// goes when the last one is answered. Otherwise it is a clean exit
    /// (hot exit keeps the unsaved work), and `true`.
    pub(crate) fn window_should_close(
        &mut self,
        ask: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // A cancelled close keeps the window: repaint the committed cell.
        cx.notify();
        if self.quitting {
            if quit_prompt_live(&self.tabs) {
                // Already asking: the X again changes nothing.
                return false;
            }
            // Its question went away some other way: start again.
            self.quit_cancelled();
        }
        commit_pending_for_exit(&mut self.tabs);
        self.persist();
        if !(ask && self.ask_on_close && self.tabs.iter().any(|t| t.dirty)) {
            // The persist above is the final one, so only the marker is left.
            self.mark_clean_exit();
            return true;
        }
        self.quitting = true;
        self.quit_discards.clear();
        self.quit_tabs = tab_ids(&self.tabs);
        self.next_quit_prompt(window, cx);
        false
    }

    /// Ask about the next unsaved tab, in tab order, or go when none is left.
    fn next_quit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if tab_ids(&self.tabs) != self.quit_tabs {
            // A tab came or went (or moved) under the questions: the answers
            // so far name tabs by index, so none of them is applied.
            self.quit_cancelled();
            self.set_status(QUIT_TABS_CHANGED);
            self.refocus(window, cx);
            return;
        }
        let next = next_to_ask(&self.tabs, &self.quit_discards);
        match next {
            None => self.finish_quit(window),
            Some(i) if self.tabs[i].dialogs.is_open() => {
                self.quit_cancelled();
                self.refuse_close_under_dialog(i, window, cx);
            }
            Some(i) => {
                if self.active != i {
                    self.active = i;
                    self.drop_grid_state();
                }
                let prompt = close_prompt(
                    &self.tabs[i],
                    true,
                    &known_locations(self.harness.is_some()),
                );
                self.tabs[i].dialogs.push(prompt);
                self.refocus(window, cx);
            }
        }
    }

    /// Cancel while quitting: the window stays. Tabs already saved stay
    /// saved; those answered Don't Save keep their unsaved work.
    fn quit_cancelled(&mut self) {
        self.quitting = false;
        self.quit_discards.clear();
        self.quit_tabs.clear();
    }

    /// Every unsaved tab is answered: keep the drafts of workbooks answered
    /// Don't Save (#613), drop never-saved tabs answered so, persist the
    /// others as their files alone, and go as a clean exit.
    fn finish_quit(&mut self, window: &mut Window) {
        let root = config_root();
        let now = std::time::SystemTime::now();
        for &i in &self.quit_discards {
            // Nowhere left to report one that could not be kept: the user
            // chose to discard, as with a tab's own Don't Save.
            let _ = keep_closed_draft(
                &root,
                &self.tabs[i],
                &CloseStep::Discard,
                self.autorecover_minutes,
                self.keep_drafts,
                now,
            );
        }
        let forget = forget_on_quit(
            &mut self.tabs,
            &mut self.active,
            std::mem::take(&mut self.quit_discards),
        );
        write_session_forgetting(&root, &self.tabs, self.active, self.prefs(), &forget);
        self.last_persist.set(std::time::Instant::now());
        self.mark_clean_exit();
        self.quitting = false;
        if self.harness.is_some() {
            // The harness ends the process once its reply is out.
            self.quit_ready = true;
        } else {
            window.remove_window();
        }
    }
}

/// What a quit cancelled because its tabs changed says.
const QUIT_TABS_CHANGED: &str =
    "The open tabs changed: close the window again to be asked about each";

/// A tab as the window's close knows it: its title and file.
pub(crate) type TabId = (SharedString, Option<PathBuf>);

/// The tabs, in order, as [`TabId`]s.
pub(crate) fn tab_ids(tabs: &[DocTab]) -> Vec<TabId> {
    tabs.iter()
        .map(|t| (t.title.clone(), t.path.clone()))
        .collect()
}

/// Whether a window close's question is open on some tab: the quit is live
/// only while one is, whatever the app last recorded.
fn quit_prompt_live(tabs: &[DocTab]) -> bool {
    tabs.iter().any(|t| {
        t.dialogs
            .top()
            .is_some_and(|d| d.owner == DialogOwner::SaveOnClose { quit: true })
    })
}

/// A Project control verb that would drop a tab's dialogs (an edit, a save,
/// a reload) or move the focus (an open) is refused while a close prompt is
/// open: the prompt would vanish under the question it asks, or the tabs
/// change under a window close's questions (#630).
pub(crate) fn close_prompt_refusal(tabs: &[DocTab], verb: &str) -> Result<(), String> {
    let drops = matches!(verb, "proj.open" | "proj.save" | "proj.reload")
        || projctl::MUTATING.contains(&verb);
    let open = tabs.iter().find_map(|t| {
        t.dialogs
            .top()
            .filter(|d| matches!(d.owner, DialogOwner::SaveOnClose { .. }))
    });
    match open {
        Some(d) if drops => Err(format!("a dialog is open: {}", d.title)),
        _ => Ok(()),
    }
}

/// The next tab the window's close asks about: the first unsaved one not
/// already answered Don't Save (`discarded`).
fn next_to_ask(tabs: &[DocTab], discarded: &[usize]) -> Option<usize> {
    (0..tabs.len()).find(|&i| tabs[i].dirty && !discarded.contains(&i))
}

/// Apply the quit's Don't Save answers (`discarded`, by index) to `tabs`: a
/// never-saved tab goes, and the indexes of the file-backed ones, whose
/// unsaved work the session forgets, are returned as they are after that.
fn forget_on_quit(tabs: &mut Vec<DocTab>, active: &mut usize, discarded: Vec<usize>) -> Vec<usize> {
    let mut forget: Vec<bool> = (0..tabs.len()).map(|i| discarded.contains(&i)).collect();
    for i in (0..tabs.len()).rev() {
        if forget[i] && tabs[i].path.is_none() {
            remove_tab(tabs, active, i);
            forget.remove(i);
        }
    }
    forget
        .iter()
        .enumerate()
        .filter_map(|(i, f)| f.then_some(i))
        .collect()
}

#[cfg(test)]
mod tests;
