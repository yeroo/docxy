//! The sheet's drop-down lists while typing (#665, #686): Pick From
//! Drop-down List (Alt+Down, the cell menu's Pick From Drop-down List...) and
//! Formula AutoComplete's list under a formula being typed.
//!
//! What a list holds comes from gridcore ([`gridcore::entry::pick_list`],
//! [`gridcore::fcomplete::completions`]); this module decides which list
//! Alt+Down opens, keeps the formula list in step with the editor, and
//! enters a chosen value through the typed commit.

use super::*;
use gridcore::fcomplete::Completions;

/// Is this keystroke Alt+Down, the drop-down key? The bare Alt before it has
/// raised the KeyTips, which would swallow the Down; `on_key` routes it to
/// the sheet instead, as it does Alt+Shift+Right/Left.
pub(crate) fn alt_down_key(key: &str, m: Modifiers) -> bool {
    key == "down" && m.alt && !m.control && !m.platform && !m.shift
}

/// Is this keystroke Alt+Backspace, the editor's revert (#1147)? Routed past
/// the KeyTips like Alt+Down.
pub(crate) fn alt_backspace_key(key: &str, m: Modifiers) -> bool {
    key == "backspace" && m.alt && !m.control && !m.platform && !m.shift
}

/// What Alt+Down opens, in Excel's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AltDown {
    /// The open editor holds a formula: Formula AutoComplete's list, on
    /// demand (FRM-153).
    Completions,
    /// The selected cell is a header with a filter button: its drop-down.
    FilterButton(u32),
    /// The selected cell has a `list` data validation: its drop-down.
    Validation,
    /// Pick From Drop-down List.
    PickList,
}

/// The list Alt+Down opens on `v`, `dv_list` telling whether the selected
/// cell has a `list` validation. While a text entry is being typed it is the
/// pick list, whatever the cell has.
pub(crate) fn alt_down_target(v: &SheetView, dv_list: bool) -> AltDown {
    if let Some(buf) = v.editing.as_deref() {
        return if buf.starts_with('=') {
            AltDown::Completions
        } else {
            AltDown::PickList
        };
    }
    if let Some(col) = v.filter_button_at_sel() {
        return AltDown::FilterButton(col);
    }
    if dv_list {
        AltDown::Validation
    } else {
        AltDown::PickList
    }
}

/// Formula AutoComplete's list as the editor shows it: the list, its
/// highlighted item, and the buffer and caret it was made for. A list made
/// for another buffer or caret is stale.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CompleteList {
    pub list: Completions,
    pub sel: usize,
    pub made_for: (String, usize),
    /// Esc closed it: it stays closed until the buffer or caret changes.
    pub closed: bool,
}

impl SheetView {
    /// The cell an edit or a pick lands in: the edit's origin, else the
    /// selection.
    fn pick_cell(&self) -> (usize, u32, u32) {
        self.edit_origin
            .filter(|_| self.editing.is_some())
            .unwrap_or((self.active, self.sel.0, self.sel.1))
    }

    /// Pick From Drop-down List's entries for the cell being edited or
    /// selected (ENT-072).
    pub(crate) fn pick_values(&self) -> Vec<String> {
        let (s, r, c) = self.pick_cell();
        self.pkg
            .workbook
            .sheets
            .get(s)
            .map(|sh| gridcore::entry::pick_list(sh, r, c))
            .unwrap_or_default()
    }

    /// Enter `value` from the pick list into the cell being edited (its
    /// text replaced) or selected, through the typed commit as a taken
    /// AutoComplete proposal, so AutoCorrect leaves the column's spelling
    /// alone. False when the commit was refused (the editor then holds it).
    pub(crate) fn pick_value(&mut self, value: &str) -> bool {
        if self.editing.is_none() {
            self.begin_cell_edit(Some(String::new()));
        }
        self.editing = Some(value.to_string());
        self.edit_caret_to_end();
        self.edit_proposal = Some((0, value.to_string()));
        self.commit_edit()
    }

    /// The column of the filter button on the selected cell, when it is a
    /// header of the sheet's AutoFilter with its button drawn.
    pub(crate) fn filter_button_at_sel(&self) -> Option<u32> {
        let sh = self.sheet();
        let af = sh.auto_filter.as_ref()?;
        let (r1, c1, _, c2) = af.range;
        let (r, c) = self.sel;
        (r == r1 && (c1..=c2).contains(&c) && !sh.row_hidden(r) && !sh.col_hidden(c)).then_some(c)
    }

    /// A fresh Formula AutoComplete list for the buffer and caret now, with
    /// the option on or on `demand` (Alt+Down: an empty prefix lists
    /// everything). The highlight stays on the item the kept list had
    /// highlighted while that is still listed.
    fn complete_fresh(&self, demand: bool) -> Option<CompleteList> {
        let buf = self.editing.as_deref()?;
        if !(demand || self.edit_opts.formula_autocomplete) {
            return None;
        }
        let (s, _, _) = self.pick_cell();
        let list =
            gridcore::fcomplete::completions(buf, self.edit_caret, &self.pkg.workbook, s, demand)?;
        let was = self
            .edit_complete
            .as_ref()
            .and_then(|c| c.list.items.get(c.sel))
            .map(|i| &i.label);
        let sel = was
            .and_then(|w| list.items.iter().position(|i| i.label == *w))
            .unwrap_or(0);
        Some(CompleteList {
            list,
            sel,
            made_for: (buf.to_string(), self.edit_caret),
            closed: false,
        })
    }

    /// Formula AutoComplete's list as it shows now (#686): the kept one while
    /// the buffer and caret are those it was made for (none once Esc closed
    /// it), else a fresh one. What the grid draws and the harness reads.
    pub(crate) fn complete_view(&self) -> Option<CompleteList> {
        let buf = self.editing.as_deref()?;
        match &self.edit_complete {
            Some(c) if c.made_for.0 == buf && c.made_for.1 == self.edit_caret => {
                (!c.closed).then(|| c.clone())
            }
            _ => self.complete_fresh(false),
        }
    }

    /// [`SheetView::complete_view`], kept, for a key to act on; with
    /// `demand` a fresh list whatever was kept.
    pub(crate) fn complete_now(&mut self, demand: bool) -> Option<&CompleteList> {
        let stale = self.edit_complete.as_ref().is_none_or(|c| {
            Some(c.made_for.0.as_str()) != self.editing.as_deref()
                || c.made_for.1 != self.edit_caret
        });
        if demand || stale {
            self.edit_complete = self.complete_fresh(demand);
            // A rebuilt list starts at the top and scrolls to the highlight
            // it carries: once, on this event (not per frame, which would
            // undo the mouse wheel).
            self.fx_scroll = ScrollHandle::new();
            if let Some(c) = &self.edit_complete {
                self.fx_scroll.scroll_to_item(c.sel);
            }
        }
        self.edit_complete.as_ref().filter(|c| !c.closed)
    }

    /// Bring the kept list up to the buffer and caret (run each frame): when
    /// typing or a caret move changed them, the list is rebuilt, its
    /// highlight carried and its scroll followed once.
    pub(crate) fn sync_complete(&mut self) {
        if self.editing.is_some() {
            self.complete_now(false);
        }
    }

    /// Whether Formula AutoComplete's list is showing.
    pub(crate) fn complete_open(&mut self) -> bool {
        self.complete_now(false).is_some()
    }

    /// Up/Down on the open list: move its highlight, at the ends staying.
    pub(crate) fn complete_step(&mut self, down: bool) {
        if let Some(c) = self.edit_complete.as_mut() {
            let last = c.list.items.len().saturating_sub(1);
            c.sel = if down {
                (c.sel + 1).min(last)
            } else {
                c.sel.saturating_sub(1)
            };
            // Event-side, so the mouse wheel is not undone by a re-render.
            self.fx_scroll.scroll_to_item(c.sel);
        }
    }

    /// Tab on the open list: the highlighted item replaces the typed prefix
    /// (a function with its `(`), and the list closes.
    pub(crate) fn complete_insert(&mut self) -> bool {
        let Some(c) = self.edit_complete.take() else {
            return false;
        };
        let (Some(buf), true) = (self.editing.as_deref(), !c.closed) else {
            return false;
        };
        let Some((text, caret)) = c.list.insert(buf, self.edit_caret, c.sel) else {
            return false;
        };
        self.editing = Some(text);
        self.edit_caret = caret;
        self.edit_proposal = None;
        self.edit_touched();
        // Closed for the buffer the insert made, until it is edited.
        self.edit_complete = Some(CompleteList {
            closed: true,
            made_for: (self.editing.clone().unwrap_or_default(), caret),
            ..c
        });
        true
    }

    /// Esc on the open list: it closes, the editor stays.
    pub(crate) fn complete_close(&mut self) {
        if let Some(c) = self.edit_complete.as_mut() {
            c.closed = true;
        }
    }
}

/// Every sheet tab's formula list, in step with its editor: the per-frame
/// beside `stamp_autocorrect`.
pub(crate) fn sync_lists(tabs: &mut [DocTab]) {
    for t in tabs {
        if let Surface::Sheet(v) = &mut t.surface {
            v.sync_complete();
        }
    }
}

impl Docxy {
    /// Alt+Down (#665, FRM-153): the drop-down the selected cell or the
    /// open editor has, in Excel's order — see [`alt_down_target`].
    pub(crate) fn sheet_alt_down(&mut self, cx: &mut Context<Self>) {
        let dv_list = self.dv_list_values().is_some();
        let Some(target) = self.active_sheet().map(|v| alt_down_target(v, dv_list)) else {
            return;
        };
        match target {
            AltDown::Completions => {
                let listed = self
                    .active_sheet_mut()
                    .is_some_and(|v| v.complete_now(true).is_some());
                if !listed {
                    self.set_status("No functions or names to offer here");
                }
                cx.notify();
            }
            AltDown::FilterButton(col) => self.sheet_filter_button(col, cx),
            AltDown::Validation => {
                if !self.protected_refused(cx) {
                    self.sheet_dv_open = true;
                }
                cx.notify();
            }
            // A refusal is in the status line already.
            AltDown::PickList => {
                let _ = self.open_pick_menu(cx);
            }
        }
    }

    /// Pick From Drop-down List's menu under the selected cell (the window's
    /// corner when the cell is off screen). An empty list opens nothing and
    /// says so; a protected sheet is refused.
    pub(crate) fn open_pick_menu(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let refuse = |this: &mut Self, why: &str, cx: &mut Context<Self>| -> Result<(), String> {
            this.set_status(why.to_string());
            cx.notify();
            Err(why.to_string())
        };
        if self.protected_refused(cx) {
            return Err("this workbook is open in Protected View".into());
        }
        if self.sheet_protected() {
            return refuse(
                self,
                "The sheet is protected: unprotect it to enter values",
                cx,
            );
        }
        let Some(v) = self.active_sheet() else {
            return Err("the active tab is not a spreadsheet".into());
        };
        let values = v.pick_values();
        if values.is_empty() {
            return refuse(self, "No entries above or below this cell to pick from", cx);
        }
        let sel = v.sel;
        let at = self
            .cells_bounds(sel, sel)
            .map(|b| b.bottom_left())
            .unwrap_or_else(|_| point(px(160.), px(160.)));
        self.open_menu(menu::MenuTarget::PickList, at, menu::pick_menu(&values), cx);
        Ok(())
    }

    /// A press on item `i` of Formula AutoComplete's list: it is
    /// highlighted and inserted, as Tab would.
    pub(crate) fn sheet_complete_pick(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(v) = self.active_sheet_mut() {
            if v.complete_now(false).is_some() {
                if let Some(c) = v.edit_complete.as_mut() {
                    c.sel = i.min(c.list.items.len().saturating_sub(1));
                }
                v.complete_insert();
            }
        }
        cx.notify();
    }

    /// Entry `i` of the pick list, chosen: entered as a typed commit.
    pub(crate) fn sheet_pick_item(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.sheet_protected() || self.protected_refused(cx) {
            return;
        }
        let Some(value) = self
            .active_sheet()
            .and_then(|v| v.pick_values().get(i).cloned())
        else {
            return;
        };
        if self
            .active_sheet_mut()
            .is_some_and(|v| v.pick_value(&value))
        {
            self.mark_sheet_dirty();
        }
        self.sheet_entry_refused(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
