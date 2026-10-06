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
/// write while it was unsaved (`last_hot`). A window close comes here only
/// for a tab answered Don't Save in its per-document questions (#630); a
/// silent one keeps every tab as a tab (hot exit).
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

/// What the sheet-rename bar's Enter commits: rename the sheet and retarget
/// the charts this session authored, which the workbook-side ref rewrite
/// (`rename_sheet`) cannot see. `false` — nothing changed — when the tab
/// takes no edits, isn't a workbook, or the name was declined (already taken).
pub(crate) fn commit_rename_buffer(tab: &mut DocTab, idx: usize, buf: &str) -> bool {
    if tab.access.locked() {
        return false;
    }
    let Surface::Sheet(v) = &mut tab.surface else {
        return false;
    };
    let old = v.pkg.workbook.sheets.get(idx).map(|s| s.name.clone());
    // `rename_sheet` follows the refs inside the workbook (and declines a
    // name already taken); a chart this session authored isn't in there yet,
    // and would save pointing at a sheet name that no longer exists.
    if !v.pkg.rename_sheet(idx, buf) {
        return false;
    }
    if let Some(old) = old {
        let new = buf.trim().to_string();
        for c in &mut v.charts {
            gridcore::edit::rename_sheet_in_chart(&mut c.data, &old, &new);
        }
    }
    tab.dirty = true;
    true
}

/// The exit path's rename commit ([`commit_rename_buffer`] minus one quirk).
/// The rename bar seeds its buffer with the sheet's current name, so a merely
/// OPEN bar holds an unchanged buffer — and `rename_sheet` takes a same-name
/// rename (its taken-name check looks at the OTHER sheets, and it trims), so
/// a sheet loaded as "Data " would be renamed to "Data", rewriting formulas
/// and dirtying the tab for nothing. The skip therefore compares both sides
/// trimmed — `rename_sheet`'s own semantics; a case-only rename still
/// commits, its trims differ. Enter's path keeps the quirk, Must-not-change.
pub(crate) fn commit_rename_buffer_for_exit(tab: &mut DocTab, idx: usize, buf: &str) -> bool {
    let unchanged = match &tab.surface {
        Surface::Sheet(v) => v
            .pkg
            .workbook
            .sheets
            .get(idx)
            .is_some_and(|s| s.name.trim() == buf.trim()),
        _ => true,
    };
    if unchanged {
        return false;
    }
    commit_rename_buffer(tab, idx, buf)
}

/// What the cell-comment bar's Enter commits: the buffer onto the selected
/// cell, an empty one deleting the comment there. `false` — nothing changed —
/// when the tab takes no edits or isn't a workbook; a damaged sheet's refusal
/// says so on the tab's status.
pub(crate) fn commit_comment_buffer(tab: &mut DocTab, author: &str, text: &str) -> bool {
    if tab.access.locked() {
        return false;
    }
    let Surface::Sheet(v) = &mut tab.surface else {
        return false;
    };
    // The view takes the undo step itself: the whole package.
    if !v.comment_cell(author, text) {
        tab.status = DAMAGED_SHEET_STATUS.into();
        return false;
    }
    tab.dirty = true;
    true
}

/// The exit path's comment commit. A CHANGED buffer commits to the CURRENT
/// SELECTION, exactly as Enter and the bar's own label do — the label reads
/// "Comment on {cell}:" off the live selection, so that is the cell the user
/// sees themselves editing. `seed` — the raw text the bar opened with —
/// serves only the skip decision, so an untouched bar never deletes or copies
/// after a click moved the selection: the commit is skipped when the buffer
/// equals the seed (raw compare — a file-loaded note keeps its whitespace, so
/// a note "note\n" seeds "note\n" and a bar opened to read it must not
/// rewrite it as "note"), or the commit could not change the TARGET cell: the
/// note on it already trims to the buffer (committing would only restamp its
/// author), or it has no note and the buffer is empty (the delete that
/// isn't). Enter's own path is [`commit_comment_buffer`], unchanged.
pub(crate) fn commit_comment_buffer_for_exit(
    tab: &mut DocTab,
    seed: &str,
    author: &str,
    text: &str,
) -> bool {
    let noop = match &tab.surface {
        Surface::Sheet(v) => {
            let trimmed = text.trim();
            text == seed
                || match v
                    .pkg
                    .comments()
                    .iter()
                    .find(|cm| cm.sheet == v.active && cm.row == v.sel.0 && cm.col == v.sel.1)
                {
                    Some(cm) => cm.text.trim() == trimmed,
                    None => trimmed.is_empty(),
                }
        }
        _ => true,
    };
    if noop {
        return false;
    }
    commit_comment_buffer(tab, author, text)
}

/// Commit a sheet's open cell editor unless it was seeded from a cell and left
/// unchanged. `commit_edit` also skips that case, but taking the buffer here
/// would close the editor; cancelled close or Save As must leave it open.
/// `Err` (the reason, also put in the tab's status) when the sheet refused the
/// entry — an unfinished formula, one over the cell limit, or one that breaks
/// its cell's data-validation rule, whatever the rule's alert style (no alert
/// is shown here, so Save and close never let a rule-breaking entry in behind
/// the user's back) — and the editor stays open with the text.
fn commit_changed_cell(tab: &mut DocTab) -> Result<(), String> {
    if let Surface::Sheet(v) = &mut tab.surface
        && v.editing.is_some()
        && !v.edit_untouched()
    {
        let changed = v.commit_edit();
        if v.editing.is_some() {
            // The alert a rule-breaking entry would raise is not shown from
            // here; nothing is left waiting on it.
            v.dv_pending = None;
            let message = v
                .entry_error
                .take()
                .unwrap_or_else(|| "the cell entry could not be committed".into());
            tab.status = message.clone().into();
            return Err(message);
        }
        tab.dirty |= changed;
        if changed {
            // A cell entry is an edit outside any document's undo history:
            // a document's Repeat record is stale after it (#618).
            crate::bump_edit_generation();
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

/// What closing `tab` does: `answer` is how a dirty tab was answered, or
/// `None` when it has not been asked yet.
fn close_step(tab: &mut DocTab, answer: Option<CloseAnswer>) -> CloseStep {
    if let Err(message) = commit_pending_for_close(tab) {
        return CloseStep::Refuse(message);
    }
    if !tab.dirty {
        return CloseStep::Remove;
    }
    match answer {
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
    // A tab that saves in place keeps its own extension, whatever it is
    // (none at all included), so an unchanged name is its own file.
    let ext = match tab.path.as_deref().filter(|_| in_place) {
        Some(p) => p
            .extension()
            .map_or_else(String::new, |e| format!(".{}", e.to_string_lossy())),
        None => ".docx".into(),
    };
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
    // A tab that saves as (read-only, repaired, converted) never proposes
    // its own file: Save would write over what was opened that way.
    let names_own = |stem: &str| {
        let own = tab.path.as_deref();
        own_dir
            .as_deref()
            .is_some_and(|dir| writes_own_file(own, Some(&dir.join(format!("{stem}{ext}")))))
    };
    let stem = if !in_place && names_own(&stem) {
        format!("{stem} (copy)")
    } else {
        stem
    };
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
/// and the tab saves in place (`in_place`), else that new file. The tab's
/// own file (`own`) when it does not save in place, an existing other file,
/// an empty name and one with characters a file name cannot hold are
/// refused.
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
    if own_file {
        // Opened read-only, repaired or converted: Save is Save As, which
        // never writes over the tab's own file from here.
        return Err(format!(
            "{} was not opened for saving in place; choose another name, or use More options...",
            file_name(&target)
        ));
    }
    if target.exists() {
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

    /// Fold the app's typed-but-uncommitted dialog buffers into the ACTIVE tab
    /// before a window close, a harness `quit` or the active tab's own close
    /// persists. The sheet cell editors, open header/footer and Project cells
    /// live on the tabs and are folded by [`commit_pending_for_exit`]; what
    /// remains lives on the app: the cell-comment bar, the sheet-rename bar
    /// and a Chart panel range field. All three commit, as their Enter would
    /// ([`ref_commit`] changes nothing on an invalid ref) — unless the buffer
    /// is the seed and untouched (a bar or field merely OPEN), in which case
    /// committing would dirty the tab, restamp a comment's author, or rebuild
    /// a chart for nothing; that skip is the rule `commit_changed_cell`
    /// already follows for the cell editor. The app's
    /// rule/format bars (conditional formatting, data validation, row height)
    /// and their range fields are NOT committed — a half-typed rule would
    /// apply formatting the user never confirmed — and drop as Esc would.
    ///
    /// The drops happen even when the close is later cancelled: at this point
    /// a window close cannot know whether its prompt will be answered or
    /// cancelled, and the user buffers above are committed rather than dropped
    /// for the same reason. A cancelled close therefore keeps the committed
    /// text (its bar gone), never a half-typed rule.
    pub(crate) fn commit_dialog_buffers_for_exit(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.sheet_comment_edit.take() {
            let author = self.comment_author();
            // The seed says what the bar opened with — the exit commit skips
            // a buffer that is it. The commit itself lands on the current
            // selection, as Enter and the bar's label do. The fallback is
            // unreachable while the seed is set wherever the buffer is.
            let seed = self
                .sheet_comment_seed
                .take()
                .unwrap_or_else(|| self.selected_comment().unwrap_or_default());
            if let Some(t) = self.tabs.get_mut(self.active) {
                commit_comment_buffer_for_exit(t, &seed, &author, &text);
            }
        }
        if let Some((idx, buf)) = self.sheet_rename.take() {
            if let Some(t) = self.tabs.get_mut(self.active) {
                commit_rename_buffer_for_exit(t, idx, &buf);
            }
        }
        if let Some(f) = self.range_edit.take() {
            if !f.target.is_bar()
                && !self.active_locked()
                // Seeded and untouched, the rule the cell editor and the
                // comment bar follow: a merely focused field holds its seed
                // text, and committing that can rebuild a file-loaded chart's
                // cached data (`chart_apply_range` re-reads the cells),
                // marking it edited and regenerating — losing — its unmodeled
                // part on save.
                && self.ref_field_seed(f.target).is_none_or(|seed| seed != f.buf)
            {
                self.ref_commit(f.target, &f.buf, cx);
            }
            // What Esc does when the field goes: its last commit's message
            // must not stand under a field nobody is in — committed or
            // dropped, the field is gone either way.
            if matches!(&self.ref_msg, Some((t, _, _)) if *t == f.target) {
                self.ref_msg = None;
            }
        }
        // The rule/format bars and their buffers are deliberately dropped
        // (see above): everything `bar_close` clears, plus the row-height
        // bar's own buffer.
        self.bar_close();
        self.sheet_rowh_edit = None;
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
        if i == self.active {
            // The dialog buffers act on the active tab; committing them is
            // what the closed tab's own prompt must see as dirty.
            self.commit_dialog_buffers_for_exit(cx);
        }
        let step = close_step(&mut self.tabs[i], answer);
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
            // Before the removal shifts the indexes, so the status reset hits
            // the tab the highlighting mode started on (#623).
            self.cancel_highlight_mode();
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
            (CloseAnswer::Save, true) => {
                // The quit's own Save may have named the tab anew.
                refresh_tab_id(&mut self.quit_tabs, &self.tabs, i);
                self.next_quit_prompt(window, cx);
            }
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
        self.commit_dialog_buffers_for_exit(cx);
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

/// Tab `i` was saved by the quit's own question: a never-saved document,
/// or one saved under a new name, has a new title and file, and that is no
/// change of the tabs the quit is asking about.
fn refresh_tab_id(ids: &mut [TabId], tabs: &[DocTab], i: usize) {
    if let (Some(id), Some(t)) = (ids.get_mut(i), tabs.get(i)) {
        *id = (t.title.clone(), t.path.clone());
    }
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
