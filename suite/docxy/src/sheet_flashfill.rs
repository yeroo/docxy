//! Flash Fill on the sheet tab (#666, ENT-104..111): Ctrl+E and Data ›
//! Flash Fill, the automatic greyed preview after the second example is
//! typed, the Flash Fill Options button's menu, and the message when there
//! is no pattern.
//!
//! The pattern comes from [`gridcore::flashfill`]; each result is committed
//! as a typed entry, all of them as one undo step. The preview and the
//! options button belong to the moment they were made for: any edit, a
//! selection move or another key retires them.

use super::*;
use crate::dialog::{ButtonRole, Dialog, DialogOwner};
use gridcore::flashfill::FlashFill;

/// The greyed preview after a typed commit (ENT-105): the fill it would
/// make, on `sheet`, while the workbook is at edit `at_gen` and the selection
/// at `sel`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FlashPreview {
    pub sheet: usize,
    pub fill: FlashFill,
    pub at_gen: u64,
    pub sel: (u32, u32),
}

/// The last Flash Fill, for its Options button (ENT-109): column `col` of
/// `sheet`, the rows it changed and those it left blank, standing while
/// the workbook is at edit `at_gen` (the fill's own undo step).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LastFlash {
    pub sheet: usize,
    pub col: u32,
    pub changed: Vec<u32>,
    pub blank: Vec<u32>,
    pub at_gen: u64,
}

impl LastFlash {
    /// The cell the Options button sits beside: the last one filled.
    pub fn button_cell(&self) -> Option<(u32, u32)> {
        self.changed.iter().max().map(|&r| (r, self.col))
    }
}

/// The Flash Fill Options menu (ENT-109).
pub(crate) fn options_menu(last: &LastFlash) -> Vec<menu::MenuItem> {
    use menu::{Entry, MenuItem::Item};
    let act = |a| Act::Sheet(a);
    vec![
        Item(Entry::new(
            "ff-undo",
            "Undo Flash Fill",
            "",
            act(SheetAct::FlashUndo),
            true,
        )),
        Item(Entry::new(
            "ff-accept",
            "Accept suggestions",
            "",
            act(SheetAct::FlashAccept),
            true,
        )),
        Item(Entry::new(
            "ff-blank",
            &format!("Select all {} blank cells", last.blank.len()),
            "",
            act(SheetAct::FlashSelectBlank),
            !last.blank.is_empty(),
        )),
        Item(Entry::new(
            "ff-changed",
            &format!("Select all {} changed cells", last.changed.len()),
            "",
            act(SheetAct::FlashSelectChanged),
            !last.changed.is_empty(),
        )),
    ]
}

/// The box spanning `rows` of `col`, as (first, last) corners; the grid's
/// selection is one rectangle, so non-adjacent rows select their span.
fn rows_box(rows: &[u32], col: u32) -> Option<((u32, u32), (u32, u32))> {
    let lo = *rows.iter().min()?;
    let hi = *rows.iter().max()?;
    Some(((lo, col), (hi, col)))
}

impl SheetView {
    /// Flash Fill the column of (row, col) on the active sheet as one undo
    /// step (Ctrl+E). The error is the message to show (ENT-110), or why the
    /// cells refused the results (part of an array).
    pub(crate) fn flash_fill_at(&mut self, row: u32, col: u32) -> Result<FlashFill, String> {
        let s = self.active;
        let fill = gridcore::flashfill::flash_fill(&self.pkg.workbook, s, row, col)
            .map_err(|e| e.message().to_string())?;
        self.flash_apply(s, &fill)?;
        Ok(fill)
    }

    /// Write `fill` into sheet `s`: each result typed, one undo step, and the
    /// Options button set up for it.
    fn flash_apply(&mut self, s: usize, fill: &FlashFill) -> Result<(), String> {
        let today = self.engine.clock;
        let snap = self.snapshot();
        let mut changes = Vec::new();
        for (r, text) in &fill.fills {
            let cell =
                gridcore::entry::entry_cell(&mut self.pkg.workbook, s, *r, fill.col, text, today)
                    .map_err(|e| e.to_string())?;
            changes.push((*r, fill.col, cell));
        }
        if self.refuses(s, &changes) {
            return Err(self
                .entry_error
                .take()
                .unwrap_or_else(|| gridcore::engine::PART_OF_ARRAY.into()));
        }
        for (r, c, cell) in changes {
            self.engine
                .set_cell(&mut self.pkg.workbook, (s, r, c), cell);
        }
        self.push_undo_snapshot(snap);
        self.flash_preview = None;
        self.last_flash = Some(LastFlash {
            sheet: s,
            col: fill.col,
            changed: fill.changed(),
            blank: fill.blank.clone(),
            at_gen: self.edit_gen,
        });
        Ok(())
    }

    /// After a typed commit into (s, r, c): the greyed preview, when
    /// Automatically Flash Fill is on and the column's examples call for one
    /// ([`gridcore::flashfill::flash_preview`]).
    pub(crate) fn flash_preview_after(&mut self, (s, r, c): (usize, u32, u32)) {
        self.flash_preview = None;
        if !self.edit_opts.flash_fill_auto || s != self.active {
            return;
        }
        if let Some(fill) = gridcore::flashfill::flash_preview(&self.pkg.workbook, s, r, c) {
            self.flash_preview = Some(FlashPreview {
                sheet: s,
                fill,
                at_gen: self.edit_gen,
                sel: self.sel,
            });
        }
    }

    /// The preview while it stands: nothing edited, the selection where the
    /// commit left it, no editor open.
    pub(crate) fn live_preview(&self) -> Option<&FlashPreview> {
        self.flash_preview.as_ref().filter(|p| {
            p.sheet == self.active
                && p.at_gen == self.edit_gen
                && p.sel == self.sel
                && self.editing.is_none()
        })
    }

    /// Enter on a standing preview: its values, written (one undo step).
    /// False when there is none.
    pub(crate) fn accept_preview(&mut self) -> bool {
        let Some(p) = self.live_preview().cloned() else {
            return false;
        };
        self.flash_apply(p.sheet, &p.fill).is_ok()
    }

    /// The last Flash Fill while its Options button stands: nothing edited
    /// since, on this sheet.
    pub(crate) fn live_flash(&self) -> Option<&LastFlash> {
        self.last_flash
            .as_ref()
            .filter(|f| f.sheet == self.active && f.at_gen == self.edit_gen)
    }

    /// Select all N blank (`blank`) or changed cells of the last fill.
    pub(crate) fn flash_select(&mut self, blank: bool) -> bool {
        let Some(f) = self.live_flash() else {
            return false;
        };
        let rows = if blank { &f.blank } else { &f.changed };
        let Some((a, b)) = rows_box(rows, f.col) else {
            return false;
        };
        self.sel = a;
        self.anchor = b;
        true
    }
}

impl Docxy {
    /// Ctrl+E and Data › Flash Fill (#666): commit an open entry first, then
    /// fill the selected cell's column. No pattern opens Excel's message
    /// (ENT-110); the status counts what changed.
    pub(crate) fn sheet_flash_fill(&mut self, cx: &mut Context<Self>) {
        if self.protected_refused(cx) {
            return;
        }
        if self.sheet_protected() {
            self.set_status("The sheet is protected: unprotect it to Flash Fill");
            cx.notify();
            return;
        }
        let editing = self.active_sheet().is_some_and(|v| v.editing.is_some());
        if editing && !self.sheet_commit_move(0, 0, cx) {
            return;
        }
        let Some(v) = self.active_sheet_mut() else {
            return;
        };
        let (r, c) = v.sel;
        match v.flash_fill_at(r, c) {
            Ok(f) => {
                self.mark_sheet_dirty();
                self.set_status(format!(
                    "Flash Fill: {} changed cells, {} blank cells",
                    f.fills.len(),
                    f.blank.len()
                ));
            }
            Err(msg) => {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.dialogs.push(Dialog::message(
                        "flash-fill",
                        "Flash Fill",
                        msg,
                        &[("OK", ButtonRole::Accept)],
                        DialogOwner::Message,
                    ));
                }
            }
        }
        cx.notify();
    }

    /// Enter on a standing preview accepts it. False when there is none.
    pub(crate) fn sheet_accept_preview(&mut self, cx: &mut Context<Self>) -> bool {
        if self.sheet_protected() {
            return false;
        }
        if !self
            .active_sheet_mut()
            .is_some_and(SheetView::accept_preview)
        {
            return false;
        }
        self.mark_sheet_dirty();
        cx.notify();
        true
    }

    /// The Flash Fill Options button's menu, by the last filled cell. An
    /// error when no fill stands.
    pub(crate) fn open_flash_menu(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let (items, cell) = {
            let v = self
                .active_sheet()
                .ok_or("the active tab is not a spreadsheet")?;
            let f = v.live_flash().ok_or("no Flash Fill to offer options for")?;
            (options_menu(f), f.button_cell())
        };
        let at = cell
            .and_then(|c| self.cells_bounds(c, c).ok())
            .map(|b| b.bottom_right())
            .unwrap_or_else(|| point(px(160.), px(160.)));
        self.open_menu(menu::MenuTarget::FlashFill, at, items, cx);
        Ok(())
    }

    /// The Flash Fill Options items (ENT-109).
    pub(crate) fn flash_option(&mut self, act: SheetAct, cx: &mut Context<Self>) {
        let Some(v) = self.active_sheet_mut() else {
            return;
        };
        if v.live_flash().is_none() {
            return;
        }
        match act {
            SheetAct::FlashUndo => {
                v.last_flash = None;
                self.sheet_undo(cx);
            }
            SheetAct::FlashAccept => v.last_flash = None,
            SheetAct::FlashSelectBlank => {
                v.flash_select(true);
            }
            SheetAct::FlashSelectChanged => {
                v.flash_select(false);
            }
            _ => {}
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
