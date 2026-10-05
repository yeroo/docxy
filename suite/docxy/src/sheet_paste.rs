//! Paste Special, the Paste gallery, the Paste Options button and the Office
//! Clipboard in the sheet tab (#669). The rules are gridcore's
//! ([`gridcore::edit::paste_special_changes`] and its extras); this is where
//! the grid takes its copy and keeps its undo steps.

use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::sheet_menus::PasteItem;
use crate::{Docxy, SheetView};
use gridcore::edit::{ClipBlock, PasteOp, PasteSpec, PasteWhat};
use gridcore::sheet::Cell;

use gridcore::edit::Area;

/// The Paste Options button after a paste of a copy: what was pasted where,
/// so a choice from its menu pastes it again another way (R3). It stands
/// only while the workbook is as that paste left it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PasteOptions {
    pub view: u64,
    pub edit_gen: u64,
    pub block: ClipBlock,
    pub at: (u32, u32),
    /// The pasted cells; the button sits at their bottom-right.
    pub rect: Area,
    pub item: PasteItem,
    /// The copy came from another workbook: no Paste Link.
    pub foreign: bool,
    /// Set the first time it does not stand: a return to the pasted range
    /// does not bring it back (#707 r2 i2).
    pub gone: std::cell::Cell<bool>,
}

impl PasteOptions {
    /// Whether the button still stands on `v`: the workbook as the paste
    /// left it, and the pasted range still the selection (R3).
    pub fn stands(&self, v: &SheetView) -> bool {
        let stands = !self.gone.get()
            && v.id == self.view
            && v.edit_gen == self.edit_gen
            && v.range() == self.rect
            && !v.multi_area();
        self.gone.set(!stands);
        stands
    }
}

/// The Office Clipboard (#669): the last copies and cuts, newest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OfficeClipboard {
    pub items: Vec<String>,
    /// The task pane is showing.
    pub open: bool,
}

/// How many items the Office Clipboard keeps, as Office's does.
pub(crate) const OFFICE_CLIPBOARD_CAP: usize = 24;

impl OfficeClipboard {
    /// A copy or a cut: newest first, the oldest dropped past the cap.
    pub fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.items.insert(0, text.to_string());
        self.items.truncate(OFFICE_CLIPBOARD_CAP);
    }

    /// What an item's tile shows: its first line, shortened.
    pub fn preview(text: &str) -> String {
        let line = text.lines().next().unwrap_or_default().replace('\t', "  ");
        let mut s: String = line.chars().take(48).collect();
        if line.chars().count() > 48 || text.lines().nth(1).is_some() {
            s.push('\u{2026}');
        }
        s
    }

    /// Paste All's text: every item in pane order, one under another.
    pub fn all_text(&self) -> String {
        self.items
            .iter()
            .map(|t| t.trim_end_matches(['\n', '\r']))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl SheetView {
    /// The selection's copy as Paste Special reads it: its cells, the source
    /// row and column of each, its column widths, its validation and its
    /// notes.
    pub(crate) fn clip_block(&self, rows: Vec<u32>, cols: Vec<u32>) -> ClipBlock {
        let s = self.active;
        let mut block = ClipBlock::capture(&self.pkg.workbook, s, rows, cols);
        block.set_notes(
            self.pkg
                .comments()
                .into_iter()
                .filter(|c| c.sheet == s && !c.threaded)
                .map(|c| (c.row, c.col, c.author, c.text)),
        );
        block
    }

    /// Clipboard text as a copy: each field read as typed into the cell it
    /// lands in at `at` ([`gridcore::entry::paste_cell`]), as a text paste
    /// reads it.
    pub(crate) fn text_block(&mut self, text: &str, at: (u32, u32)) -> Vec<Vec<Cell>> {
        let s = self.active;
        let ctx = gridcore::entry::entry_ctx(&self.pkg.workbook, self.engine.clock);
        let wb = &mut self.pkg.workbook;
        let mut block = Vec::new();
        for (dr, line) in text
            .replace("\r\n", "\n")
            .trim_end_matches('\n')
            .split('\n')
            .enumerate()
        {
            let mut row = Vec::new();
            for (dc, f) in line.split('\t').enumerate() {
                let style = wb.sheets[s]
                    .cell(at.0 + dr as u32, at.1 + dc as u32)
                    .map_or(0, |cl| cl.style);
                row.push(gridcore::entry::paste_cell(&mut wb.styles, style, f, &ctx));
            }
            block.push(row);
        }
        block
    }

    /// Clipboard text as a [`ClipBlock`], for Paste Special's All, Values and
    /// Transpose of text (no formulas to translate).
    pub(crate) fn text_clip_block(&mut self, text: &str, at: (u32, u32)) -> ClipBlock {
        let cells = self.text_block(text, at);
        let w = cells.iter().map(Vec::len).max().unwrap_or(0);
        let mut cells = cells;
        for row in &mut cells {
            row.resize(w, Cell::default());
        }
        ClipBlock {
            rows: (0..cells.len() as u32).collect(),
            cols: (0..w as u32).collect(),
            sheet: self.active,
            sheet_name: self.sheet().name.clone(),
            widths: vec![gridcore::sheet::DEFAULT_COL_WIDTH; w],
            cells,
            notes: Vec::new(),
            rules: Vec::new(),
        }
    }

    /// Paste Special `block` at `at` as one undo step (a package step when it
    /// writes notes or validation, which live in the parts), the pasted range
    /// selected. Refused, with nothing changed, over part of an array.
    pub(crate) fn paste_special_at(
        &mut self,
        block: &ClipBlock,
        spec: &PasteSpec,
        at: (u32, u32),
    ) -> Result<Area, String> {
        let s = self.active;
        if self.sheet().is_protected() {
            return Err(crate::sheet_goto::SHEET_PROTECTED.into());
        }
        // The styles a paste interns are taken back when it is refused, so a
        // refusal leaves nothing behind.
        let styles = self.pkg.workbook.styles.clone();
        let changes =
            match gridcore::edit::paste_special_changes(&mut self.pkg.workbook, s, at, block, spec)
            {
                Ok(changes) => changes,
                Err(why) => {
                    self.pkg.workbook.styles = styles;
                    return Err(why.into());
                }
            };
        let extras = gridcore::edit::paste_special_extras(block, at, spec);
        if self.refuses(s, &changes) {
            self.pkg.workbook.styles = styles;
            return Err(gridcore::engine::PART_OF_ARRAY.into());
        }
        // The undo step holds the styles as they were before the paste.
        let interned = std::mem::replace(&mut self.pkg.workbook.styles, styles);
        let package = !extras.notes.is_empty() || !extras.rules.is_empty();
        if package {
            self.push_undo_snapshot(self.snapshot_package());
        } else {
            self.push_undo();
        }
        self.pkg.workbook.styles = interned;
        self.engine
            .set_cells_prechecked(&mut self.pkg.workbook, s, changes);
        let sheet = &mut self.pkg.workbook.sheets[s];
        for &(c, w) in &extras.widths {
            sheet.set_col_width(c, w);
        }
        if let Some(rect) = extras.clear_rules {
            gridcore::edit::clear_validation(sheet, rect);
        }
        // Rules and notes each go in with one rewrite of their parts, not
        // one per rule or note (#707 r6).
        let rules: Vec<gridcore::xlsx::NewValidation> = extras
            .rules
            .iter()
            .map(|(rect, rule)| gridcore::xlsx::NewValidation {
                range: *rect,
                kind: &rule.kind,
                operator: &rule.operator,
                formula1: &rule.formula1,
                formula2: (!rule.formula2.is_empty()).then_some(rule.formula2.as_str()),
            })
            .collect();
        self.pkg.add_data_validations(s, &rules);
        // A pasted note replaces whatever comment its cell had, a thread
        // included, as in Excel (#707 r7 M1).
        let noted: Vec<(u32, u32)> = extras.notes.iter().map(|n| (n.0, n.1)).collect();
        self.pkg.remove_comments(s, &noted);
        self.pkg.set_comments(s, &extras.notes);
        let rect = block.pasted_rect(at, spec.transpose);
        self.sel = (rect.0, rect.1);
        self.anchor = (rect.2, rect.3);
        self.clear_areas();
        Ok(rect)
    }

    /// Paste Link `block` at `at`, as one undo step.
    pub(crate) fn paste_link_at(
        &mut self,
        block: &ClipBlock,
        at: (u32, u32),
    ) -> Result<Area, String> {
        let s = self.active;
        if self.sheet().is_protected() {
            return Err(crate::sheet_goto::SHEET_PROTECTED.into());
        }
        let changes = gridcore::edit::paste_link_changes(&self.pkg.workbook, s, at, block);
        if self.refuses(s, &changes) {
            return Err(gridcore::engine::PART_OF_ARRAY.into());
        }
        self.push_undo();
        self.engine
            .set_cells_prechecked(&mut self.pkg.workbook, s, changes);
        let rect = block.pasted_rect(at, false);
        self.sel = (rect.0, rect.1);
        self.anchor = (rect.2, rect.3);
        self.clear_areas();
        Ok(rect)
    }

    /// A gallery item (or a Paste Options choice) pasting `block` at `at`.
    pub(crate) fn paste_item_at(
        &mut self,
        block: &ClipBlock,
        item: PasteItem,
        at: (u32, u32),
    ) -> Result<Area, String> {
        match item.spec() {
            Some(spec) => self.paste_special_at(block, &spec, at),
            None => self.paste_link_at(block, at),
        }
    }

    /// Paste Options: paste what `opts` pasted again as `item`, in place of
    /// the first paste: its undo step is taken back and the new paste takes
    /// it, so one undo returns to before either (R3).
    pub(crate) fn repaste(&mut self, opts: &PasteOptions, item: PasteItem) -> Result<Area, String> {
        if !opts.stands(self) {
            return Err("The paste has changed since; there is nothing to redo".into());
        }
        let (block, at) = (&opts.block, opts.at);
        self.redo_last_step(|v| v.paste_item_at(block, item, at))
            .ok_or_else(|| "There is no paste to redo".to_string())?
    }
}

/// Why `src` cannot paste as `item`: a cut takes plain Paste only (M5), text
/// only Paste, Values and Transpose, and a copy from another workbook no
/// Paste Link (M1).
pub(crate) fn item_refusal(src: &PasteSource, item: PasteItem) -> Result<(), &'static str> {
    if src.cut && item != PasteItem::Paste {
        return Err(CUT_PASTE_ONLY);
    }
    if !src.clip && !item.takes_text() {
        return Err("Only Paste, Values and Transpose paste text copied from another program");
    }
    if src.foreign && item == PasteItem::Link {
        return Err(LINK_FOREIGN);
    }
    Ok(())
}

const WHATS: [PasteWhat; 10] = PasteWhat::DIALOG;

/// Paste Special… (Ctrl+Alt+V): the Paste and Operation groups, Skip blanks,
/// Transpose and Paste Link. `clip` is whether a copy is live: without one
/// only plain text is on the clipboard, which takes All, Values and
/// Transpose, and Paste Link is off.
pub(crate) fn paste_special_dialog(clip: bool) -> Dialog {
    let mut d = Dialog::message(
        "paste-special",
        "Paste Special",
        String::new(),
        &[
            ("Paste Link", ButtonRole::Accept),
            ("OK", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::PasteSpecial { clip },
    );
    d.text = None;
    d.buttons = d
        .buttons
        .into_iter()
        .map(|b| Button {
            default: b.label == "OK",
            enabled: clip || b.label != "Paste Link",
            ..b
        })
        .collect();
    let mut what = Control::new("paste", "Paste", ControlKind::Radio, Value::Choice(Some(0)));
    what.items = WHATS.iter().map(|w| w.label().to_string()).collect();
    let mut op = Control::new(
        "operation",
        "Operation",
        ControlKind::Radio,
        Value::Choice(Some(0)),
    );
    op.items = PasteOp::ALL.iter().map(|o| o.label().to_string()).collect();
    op.enabled = clip;
    let mut blanks = Control::new(
        "skip-blanks",
        "Skip blanks",
        ControlKind::Checkbox,
        Value::Bool(false),
    );
    blanks.enabled = clip;
    d.controls = vec![
        what,
        op,
        blanks,
        Control::new(
            "transpose",
            "Transpose",
            ControlKind::Checkbox,
            Value::Bool(false),
        ),
    ];
    d.mark_opened();
    d
}

fn choice(d: &Dialog, name: &str) -> Option<usize> {
    d.controls
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| match c.value {
            Value::Choice(i) => i,
            _ => None,
        })
}

fn checked(d: &Dialog, name: &str) -> bool {
    d.controls
        .iter()
        .any(|c| c.name == name && c.value == Value::Bool(true))
}

/// What the Paste Special dialog `d` stages; refused for an option plain
/// text cannot take.
pub(crate) fn staged_spec(d: &Dialog) -> Result<PasteSpec, String> {
    let DialogOwner::PasteSpecial { clip } = d.owner else {
        return Err("not the Paste Special dialog".into());
    };
    let spec = PasteSpec {
        what: WHATS[choice(d, "paste").unwrap_or(0).min(WHATS.len() - 1)],
        op: PasteOp::ALL[choice(d, "operation")
            .unwrap_or(0)
            .min(PasteOp::ALL.len() - 1)],
        skip_blanks: checked(d, "skip-blanks"),
        transpose: checked(d, "transpose"),
    };
    if !clip
        && (!matches!(spec.what, PasteWhat::All | PasteWhat::Values)
            || spec.op != PasteOp::None
            || spec.skip_blanks)
    {
        return Err("Only All, Values and Transpose paste text copied from another program".into());
    }
    Ok(spec)
}

fn presses(d: &Dialog, button: &str, label: &str) -> bool {
    button.replace('&', "").trim().eq_ignore_ascii_case(label)
        && d.buttons.iter().any(|b| b.label == label && b.enabled)
}

/// What a Paste Special, a gallery item or Paste Options pastes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PasteSource {
    pub block: ClipBlock,
    /// A live copy (else the clipboard's text).
    pub clip: bool,
    /// The copy is a cut: only plain Paste takes it, as in Excel.
    pub cut: bool,
    /// The copy came from another workbook: Paste Link would name a sheet
    /// of this one (#707 r1 M1).
    pub foreign: bool,
}

/// Why Paste Link refuses a copy from another workbook.
pub(crate) const LINK_FOREIGN: &str =
    "Paste Link links within the workbook the cells were copied from";

/// Why Paste Special and the gallery refuse a cut.
pub(crate) const CUT_PASTE_ONLY: &str = "A cut pastes with Paste only (Ctrl+V)";

impl Docxy {
    /// What a paste would take now ([`PasteSource`]): the live copy, with
    /// whether it is a cut and whether it came from another workbook, or the
    /// clipboard's text read at the selection. `None` when there is nothing
    /// to paste.
    pub(crate) fn paste_source(&mut self, cx: &mut gpui::Context<Self>) -> Option<PasteSource> {
        let now = self.clipboard_read(cx);
        let here = self.active_sheet().map(|v| v.id);
        if let Some(clip) = self.grid_clip_live(&now) {
            return Some(PasteSource {
                block: clip.block.clone(),
                clip: true,
                cut: clip.cut,
                foreign: Some(clip.view) != here,
            });
        }
        if self
            .grid_clip
            .as_ref()
            .is_some_and(|clip| clip.spent_here(&now))
        {
            return None;
        }
        let crate::ClipRead::Text(text) = now else {
            return None;
        };
        let v = self.active_sheet_mut()?;
        let (r, c, _, _) = v.range();
        Some(PasteSource {
            block: v.text_clip_block(&text, (r, c)),
            clip: false,
            cut: false,
            foreign: false,
        })
    }

    /// A Paste gallery item over the selection (#669).
    pub(crate) fn sheet_paste_as(&mut self, item: PasteItem, cx: &mut gpui::Context<Self>) {
        if item == PasteItem::Paste {
            // The gallery's Paste is Ctrl+V's: tiles, moves a cut.
            self.sheet_paste(cx);
            return;
        }
        if self.sheet_protected() || self.protected_refused(cx) {
            return;
        }
        self.grid_clip_expire();
        let Some(src) = self.paste_source(cx) else {
            self.set_status("There is nothing to paste");
            return cx.notify();
        };
        if let Err(why) = item_refusal(&src, item) {
            self.set_status(why);
            return cx.notify();
        }
        self.sheet_paste_block(src, item, cx);
    }

    /// Paste `src` as `item` at the selection's top-left, leaving the Paste
    /// Options button after a copy's paste (not a text paste's).
    fn sheet_paste_block(
        &mut self,
        src: PasteSource,
        item: PasteItem,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(v) = self.active_sheet_mut() else {
            return;
        };
        let (r, c, _, _) = v.range();
        match v.paste_item_at(&src.block, item, (r, c)) {
            Ok(rect) => {
                let opts = src.clip.then_some(PasteOptions {
                    view: v.id,
                    edit_gen: v.edit_gen,
                    foreign: src.foreign,
                    gone: Default::default(),
                    block: src.block,
                    at: (r, c),
                    rect,
                    item,
                });
                self.paste_options = opts;
                self.grid_clip_restamp();
                self.mark_sheet_dirty();
            }
            Err(e) => self.set_status(e),
        }
        cx.notify();
    }

    /// A Paste Options choice: the last paste again, as `item`.
    pub(crate) fn sheet_paste_again(&mut self, item: PasteItem, cx: &mut gpui::Context<Self>) {
        let Some(opts) = self.paste_options.clone() else {
            self.set_status("There is no paste to change");
            return cx.notify();
        };
        if item == PasteItem::Link && opts.foreign {
            self.set_status(LINK_FOREIGN);
            return cx.notify();
        }
        let Some(v) = self.active_sheet_mut() else {
            return;
        };
        match v.repaste(&opts, item) {
            Ok(rect) => {
                self.paste_options = Some(PasteOptions {
                    edit_gen: v.edit_gen,
                    rect,
                    item,
                    ..opts
                });
                self.grid_clip_restamp();
                self.mark_sheet_dirty();
            }
            Err(e) => {
                self.paste_options = None;
                self.set_status(e);
            }
        }
        cx.notify();
    }

    /// The Paste Options button, while the paste it stands for is what the
    /// workbook still holds.
    pub(crate) fn paste_options_live(&self) -> Option<&PasteOptions> {
        let v = self.active_sheet()?;
        self.paste_options.as_ref().filter(|o| o.stands(v))
    }

    /// Paste Special… (Ctrl+Alt+V): the dialog, when there is something to
    /// paste. What it pastes is taken now, as the clipboard holds it.
    pub(crate) fn open_paste_special(&mut self, cx: &mut gpui::Context<Self>) {
        self.grid_clip_expire();
        let Some(src) = self.paste_source(cx) else {
            self.set_status("There is nothing to paste");
            return cx.notify();
        };
        if src.cut {
            self.set_status(CUT_PASTE_ONLY);
            return cx.notify();
        }
        let clip = src.clip;
        self.paste_special_source = Some(src);
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.dialogs.push(paste_special_dialog(clip));
        }
        cx.notify();
    }

    /// The Paste Special dialog's OK and Paste Link, which need the app's
    /// copy; taken before the tab's own path.
    pub(crate) fn paste_dialog_click(&mut self, button: &str) -> Option<Result<(), String>> {
        let top = self.tabs.get(self.active)?.dialogs.top()?;
        if !matches!(top.owner, DialogOwner::PasteSpecial { .. }) {
            return None;
        }
        let link = presses(top, button, "Paste Link");
        if !link && !presses(top, button, "OK") {
            return None;
        }
        let spec = if link {
            None
        } else {
            match staged_spec(top) {
                Ok(s) => Some(s),
                Err(e) => return Some(Err(e)),
            }
        };
        let Some(src) = self.paste_special_source.clone() else {
            return Some(Err("There is nothing to paste".into()));
        };
        if link {
            if let Err(why) = item_refusal(&src, PasteItem::Link) {
                return Some(Err(why.into()));
            }
        }
        let block = src.block;
        if self.protected_view() {
            return Some(Err(crate::open_mode::PROTECTED_STATUS.into()));
        }
        if self.active_sheet().is_some_and(SheetView::multi_area) {
            return Some(Err(gridcore::edit::MULTI_SELECTION.into()));
        }
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.dialogs.pop();
        }
        self.paste_special_source = None;
        let v = self.active_sheet_mut()?;
        let (r, c, _, _) = v.range();
        let done = match spec {
            Some(spec) => v.paste_special_at(&block, &spec, (r, c)),
            // `item_refusal` has refused Paste Link for anything but a copy
            // from this workbook.
            None => v.paste_link_at(&block, (r, c)),
        };
        Some(done.map(|_| {
            self.grid_clip_restamp();
            self.mark_sheet_dirty();
        }))
    }

    /// The Office Clipboard's item `i` pasted at the selection, as values
    /// (its text).
    pub(crate) fn office_paste(
        &mut self,
        i: Option<usize>,
        cx: &mut gpui::Context<Self>,
    ) -> Result<(), String> {
        let text = match i {
            Some(i) => self
                .office_clip
                .items
                .get(i)
                .cloned()
                .ok_or_else(|| format!("the Office Clipboard has no item {i}"))?,
            None => self.office_clip.all_text(),
        };
        if text.is_empty() {
            return Err("The Office Clipboard is empty".into());
        }
        if self.sheet_protected() || self.protected_refused(cx) || self.multi_area_refused(cx) {
            return Ok(());
        }
        if !self.paste_text(&text, cx) {
            return Err("The paste was refused".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "sheet_paste_tests.rs"]
mod tests;
