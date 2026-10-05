//! Dragging cells by the selection's border (#670): a drag moves them (the
//! references to them follow, as a cut's do), Ctrl held at the release
//! copies them, a right-drag asks what to do with a menu, and a drop on
//! other data asks first. One undo step each.

use crate::dialog::{ButtonRole, Dialog, DialogOwner};
use crate::sheet_menus::DropChoice;
use crate::{Docxy, GridPasteError, SheetView};
use gridcore::edit::{PasteSpec, PasteWhat};

type Rect = (u32, u32, u32, u32);

/// Excel's question before a drop overwrites data.
pub(crate) const REPLACE_DATA: &str = "There's already data here. Do you want to replace it?";

/// A drag by the selection's border in flight: the block, the cell the
/// press grabbed it by and the cell the pointer is over now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BorderDrag {
    pub src: Rect,
    pub grab: (u32, u32),
    pub over: (u32, u32),
    pub ctrl: bool,
    pub right: bool,
    /// The sheet view it started in: a release anywhere else does nothing.
    pub view: u64,
}

impl BorderDrag {
    /// Where the block's top-left lands, kept on the grid.
    pub fn dest_at(&self) -> (u32, u32) {
        let (r0, c0, r1, c1) = self.src;
        let shift = |v: u32, from: u32, to: u32, len: u32, max: u32| -> u32 {
            let v = i64::from(v) + i64::from(to) - i64::from(from);
            v.clamp(0, i64::from(max - len)) as u32
        };
        (
            shift(
                r0,
                self.grab.0,
                self.over.0,
                r1 - r0,
                gridcore::sheet::MAX_ROWS - 1,
            ),
            shift(
                c0,
                self.grab.1,
                self.over.1,
                c1 - c0,
                gridcore::sheet::MAX_COLS - 1,
            ),
        )
    }

    /// The rectangle the block would cover where it is now.
    pub fn dest(&self) -> Rect {
        let (r0, c0, r1, c1) = self.src;
        let (dr, dc) = self.dest_at();
        (dr, dc, dr + r1 - r0, dc + c1 - c0)
    }
}

impl SheetView {
    /// Whether dropping `src` on `dest` would overwrite data outside `src`.
    pub(crate) fn drop_hits_data(&self, src: Rect, dest: Rect) -> bool {
        let (r0, c0, r1, c1) = dest;
        self.sheet()
            .cells
            .range((r0, 0)..=(r1, u32::MAX))
            .any(|(&(r, c), cell)| {
                let in_src = (src.0..=src.2).contains(&r) && (src.1..=src.3).contains(&c);
                (c0..=c1).contains(&c)
                    && !in_src
                    && (!cell.value.is_empty() || cell.formula.is_some())
            })
    }

    /// Drop the block `src` with its top-left at `at`, as `choice` says:
    /// moved (references follow), copied, copied as values or formats, or
    /// linked. One undo step; `Ok(false)` for Cancel or a drop in place.
    pub(crate) fn border_drop(
        &mut self,
        src: Rect,
        at: (u32, u32),
        choice: DropChoice,
    ) -> Result<bool, String> {
        if choice == DropChoice::Cancel || at == (src.0, src.1) {
            return Ok(false);
        }
        if self.sheet().is_protected() {
            return Err(crate::sheet_goto::SHEET_PROTECTED.into());
        }
        self.anchor = (src.0, src.1);
        self.sel = (src.2, src.3);
        self.clear_areas();
        let refused = |e: GridPasteError| match e {
            GridPasteError::Refused(why) => why,
            GridPasteError::CutCancelled => crate::CUT_CANCELLED_STATUS.to_string(),
        };
        match choice {
            DropChoice::Move => {
                let clip = self.grid_clip(true).map_err(str::to_string)?;
                self.anchor = at;
                self.sel = at;
                self.paste_move(&clip).map_err(refused)?;
            }
            DropChoice::Copy => {
                let clip = self.grid_clip(false).map_err(str::to_string)?;
                self.anchor = at;
                self.sel = at;
                self.paste_copy(&clip).map_err(refused)?;
            }
            DropChoice::CopyValues | DropChoice::CopyFormats | DropChoice::Link => {
                let block = self.clip_block((src.0..=src.2).collect(), (src.1..=src.3).collect());
                match choice {
                    DropChoice::Link => self.paste_link_at(&block, at)?,
                    DropChoice::CopyValues => {
                        self.paste_special_at(&block, &PasteSpec::of(PasteWhat::Values), at)?
                    }
                    _ => self.paste_special_at(&block, &PasteSpec::of(PasteWhat::Formats), at)?,
                };
            }
            DropChoice::Cancel => unreachable!("returned above"),
        }
        Ok(true)
    }
}

/// The grid's drags waiting on something (#670, #668): a border drag in
/// flight, a drop waiting on its menu or its replace question, and a fill
/// handle's right-drag waiting on its menu. Kept apart from the app so the
/// rules for ending and dropping them are tested without a window.
#[derive(Default)]
pub(crate) struct GridDrops {
    pub border_drag: Option<BorderDrag>,
    pub border_pending: Option<BorderDrag>,
    pub border_pending_choice: DropChoice,
    pub fill_drop: Option<crate::FillDrag>,
}

/// What a border drag's release does ([`GridDrops::end_border`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BorderEnd {
    /// Nothing: no drag, a drop in place, or a release in another view than
    /// the drag started in (#707 r2 M4).
    Nothing,
    /// A right-drag: its menu opens, the drag pending on it.
    Menu,
    /// Drop it so: Move, or Copy with Ctrl held.
    Drop(BorderDrag, DropChoice),
}

impl GridDrops {
    /// A tab switch, or anything else that leaves the grid: every drag and
    /// pending drop goes.
    pub fn clear(&mut self) {
        *self = GridDrops::default();
    }

    /// A new gesture starts: a drop still waiting on its menu or question
    /// is over.
    pub fn new_gesture(&mut self) {
        self.border_pending = None;
        self.fill_drop = None;
    }

    /// A right button let go off the grid: a right-drag of the selection's
    /// border, or of the fill handle (`fill`), is cancelled with no menu,
    /// as Excel drops it (#707 r2 M4). Whether one was.
    pub fn cancel_right(&mut self, fill: &mut Option<crate::FillDrag>) -> bool {
        let mut cancelled = false;
        if fill.is_some_and(|f| f.right) {
            *fill = None;
            cancelled = true;
        }
        if self.border_drag.is_some_and(|d| d.right) {
            self.border_drag = None;
            cancelled = true;
        }
        cancelled
    }

    /// The border drag's release over the sheet view `active` (`None` off
    /// a sheet).
    pub fn end_border(&mut self, active: Option<u64>) -> BorderEnd {
        let Some(d) = self.border_drag.take() else {
            return BorderEnd::Nothing;
        };
        if !crate::gesture_stands(d.view, active) || d.dest_at() == (d.src.0, d.src.1) {
            return BorderEnd::Nothing;
        }
        if d.right {
            self.border_pending = Some(d);
            return BorderEnd::Menu;
        }
        BorderEnd::Drop(
            d,
            if d.ctrl {
                DropChoice::Copy
            } else {
                DropChoice::Move
            },
        )
    }
}

/// The replace question, before a drop overwrites data.
fn replace_question() -> Dialog {
    let mut d = Dialog::message(
        "drop-replace",
        "Microsoft Excel",
        REPLACE_DATA.to_string(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::DropReplace,
    );
    d.mark_opened();
    d
}

impl Docxy {
    /// A press on the selection's border: the block starts to move (#670).
    /// Refused on a protected sheet and for a multi-area selection; never
    /// while another grid gesture is in flight (the edge strips follow the
    /// selection, so a sweep passes under them).
    pub(crate) fn border_drag_start(
        &mut self,
        grab: (u32, u32),
        right: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.grid_gesture_in_flight()
            || self.sheet_fill.is_some()
            || self.drops.border_drag.is_some()
        {
            return;
        }
        // A new gesture: a drop still waiting on its menu or question is over.
        self.drops.new_gesture();
        if self.protected_refused(cx) || self.multi_area_refused(cx) {
            return;
        }
        if self.sheet_protected() {
            self.set_status(crate::sheet_goto::SHEET_PROTECTED);
            return cx.notify();
        }
        let Some(v) = self.active_sheet() else {
            return;
        };
        self.drops.border_drag = Some(BorderDrag {
            src: v.range(),
            grab,
            over: grab,
            ctrl: false,
            right,
            view: v.id,
        });
        cx.notify();
    }

    /// The pointer over a cell with a border drag in flight.
    pub(crate) fn border_drag_over(
        &mut self,
        cell: (u32, u32),
        ctrl: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(d) = self.drops.border_drag.as_mut() else {
            return;
        };
        if d.over != cell || d.ctrl != ctrl {
            d.over = cell;
            d.ctrl = ctrl;
            cx.notify();
        }
    }

    /// The release: a left drag moves (Ctrl copies), asking first when the
    /// drop lands on data; a right drag opens the drop menu.
    pub(crate) fn border_drag_end(&mut self, cx: &mut gpui::Context<Self>) {
        let active = self.active_sheet().map(|v| v.id);
        match self.drops.end_border(active) {
            BorderEnd::Nothing => cx.notify(),
            BorderEnd::Menu => {
                let at = self.last_pointer;
                self.open_menu(
                    crate::menu::MenuTarget::Grid(crate::menu::GridMenu::BorderDrop),
                    at,
                    crate::sheet_menus::drop_menu(),
                    cx,
                );
            }
            BorderEnd::Drop(d, choice) => self.border_choose(d, choice, cx),
        }
    }

    /// Drop `d` as `choice`, asking first when it would overwrite data.
    fn border_choose(&mut self, d: BorderDrag, choice: DropChoice, cx: &mut gpui::Context<Self>) {
        let hits = choice != DropChoice::Cancel
            && self
                .active_sheet()
                .is_some_and(|v| v.drop_hits_data(d.src, d.dest()));
        if hits {
            self.drops.border_pending = Some(d);
            self.drops.border_pending_choice = choice;
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.dialogs.push(replace_question());
            }
            return cx.notify();
        }
        self.border_apply(d, choice);
        cx.notify();
    }

    fn border_apply(&mut self, d: BorderDrag, choice: DropChoice) {
        let Some(v) = self.active_sheet_mut() else {
            return;
        };
        match v.border_drop(d.src, d.dest_at(), choice) {
            Ok(true) => self.mark_sheet_dirty(),
            Ok(false) => {}
            Err(e) => self.set_status(e),
        }
    }

    /// A choice from the right-drag's drop menu.
    pub(crate) fn border_drop_choice(&mut self, choice: DropChoice, cx: &mut gpui::Context<Self>) {
        let Some(d) = self.drops.border_pending.take() else {
            self.set_status("There is no drag to drop");
            return cx.notify();
        };
        self.border_choose(d, choice, cx);
    }

    /// The replace question's OK (the drop goes ahead) or Cancel.
    pub(crate) fn drop_dialog_click(&mut self, button: &str) -> Option<Result<(), String>> {
        let top = self.tabs.get(self.active)?.dialogs.top()?;
        if top.owner != DialogOwner::DropReplace {
            return None;
        }
        let ok = button.trim().eq_ignore_ascii_case("OK");
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.dialogs.pop();
        }
        let pending = self.drops.border_pending.take();
        if let (true, Some(d)) = (ok, pending) {
            let choice = self.drops.border_pending_choice;
            self.border_apply(d, choice);
        }
        Some(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Surface, new_sheet_surface};
    use gridcore::sheet::{Cell, CellValue, parse_cell_name};

    fn view() -> SheetView {
        let Surface::Sheet(v) = new_sheet_surface() else {
            panic!("a new sheet surface")
        };
        v
    }

    fn at(name: &str) -> (u32, u32) {
        parse_cell_name(name).unwrap()
    }

    fn put(v: &mut SheetView, name: &str, cell: Cell) {
        let (r, c) = at(name);
        let s = v.active;
        v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
    }

    fn value(v: &SheetView, name: &str) -> CellValue {
        let (r, c) = at(name);
        v.sheet()
            .cell(r, c)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    fn formula(v: &SheetView, name: &str) -> Option<String> {
        let (r, c) = at(name);
        v.sheet().cell(r, c).and_then(|c| c.formula.clone())
    }

    fn drag(right: bool, view: u64) -> BorderDrag {
        BorderDrag {
            src: (0, 0, 0, 0),
            grab: (0, 0),
            over: (3, 3),
            ctrl: false,
            right,
            view,
        }
    }

    /// #707 r3 m4: a right-drag let go off the grid is cancelled, a fill
    /// handle's too; a left one is not.
    #[test]
    fn a_right_release_off_the_grid_cancels_right_drags_only() {
        let mut drops = GridDrops {
            border_drag: Some(drag(true, 1)),
            ..Default::default()
        };
        let mut fill = Some(crate::FillDrag {
            src: (0, 0, 0, 0),
            to: (2, 0),
            ctrl: false,
            right: true,
            view: 1,
        });
        assert!(drops.cancel_right(&mut fill));
        assert!(drops.border_drag.is_none() && fill.is_none());
        drops.border_drag = Some(drag(false, 1));
        assert!(!drops.cancel_right(&mut None));
        assert!(
            drops.border_drag.is_some(),
            "a left drag ends at its own release"
        );
    }

    /// #707 r3 m4: a tab switch drops everything pending.
    #[test]
    fn leaving_the_grid_clears_every_pending_drop() {
        let mut drops = GridDrops {
            border_drag: Some(drag(true, 1)),
            border_pending: Some(drag(true, 1)),
            border_pending_choice: DropChoice::Link,
            fill_drop: Some(crate::FillDrag {
                src: (0, 0, 0, 0),
                to: (2, 0),
                ctrl: false,
                right: true,
                view: 1,
            }),
        };
        drops.clear();
        assert!(drops.border_drag.is_none() && drops.border_pending.is_none());
        assert!(drops.fill_drop.is_none());
        assert_eq!(drops.border_pending_choice, DropChoice::Move);
    }

    /// #707 r3 m4: a border drag released in another view does nothing (no
    /// menu, no drop); in its own, a right-drag waits on its menu and a
    /// left one moves (Ctrl copies).
    #[test]
    fn a_border_drag_ends_only_in_its_own_view() {
        let mut drops = GridDrops {
            border_drag: Some(drag(true, 1)),
            ..Default::default()
        };
        assert_eq!(drops.end_border(Some(2)), BorderEnd::Nothing);
        assert!(drops.border_pending.is_none(), "no menu waits");
        drops.border_drag = Some(drag(true, 1));
        assert_eq!(drops.end_border(None), BorderEnd::Nothing, "a document tab");
        drops.border_drag = Some(drag(true, 1));
        assert_eq!(drops.end_border(Some(1)), BorderEnd::Menu);
        assert!(drops.border_pending.is_some());
        let mut ctrl = drag(false, 1);
        ctrl.ctrl = true;
        drops.border_drag = Some(ctrl);
        assert_eq!(
            drops.end_border(Some(1)),
            BorderEnd::Drop(ctrl, DropChoice::Copy)
        );
        drops.new_gesture();
        assert!(drops.border_pending.is_none());
    }

    /// #707 r2 M4: a drag released in another tab (or a document) than the
    /// sheet it started in does nothing.
    #[test]
    fn a_gesture_ends_only_in_the_view_it_started_in() {
        assert!(crate::gesture_stands(7, Some(7)));
        assert!(!crate::gesture_stands(7, Some(8)), "another workbook tab");
        assert!(!crate::gesture_stands(7, None), "a document tab");
    }

    #[test]
    fn the_drop_lands_where_the_grab_moved() {
        let d = BorderDrag {
            src: (1, 1, 2, 2),
            grab: (2, 1),
            over: (5, 4),
            ctrl: false,
            right: false,
            view: 0,
        };
        assert_eq!(d.dest_at(), (4, 4));
        assert_eq!(d.dest(), (4, 4, 5, 5));
        // Never off the grid's top or left edge.
        let d = BorderDrag { over: (0, 0), ..d };
        assert_eq!(d.dest_at(), (0, 0));
    }

    #[test]
    fn a_move_takes_its_dependents_along_as_one_step() {
        let mut v = view();
        put(&mut v, "A1", Cell::number(1.0));
        put(&mut v, "B1", Cell::formula("A1*10"));
        put(&mut v, "H1", Cell::formula("A1+B1"));
        assert_eq!(
            v.border_drop((0, 0, 0, 1), at("A10"), DropChoice::Move),
            Ok(true)
        );
        assert_eq!(value(&v, "A1"), CellValue::Empty);
        assert_eq!(formula(&v, "B10").as_deref(), Some("A10*10"));
        assert_eq!(formula(&v, "H1").as_deref(), Some("A10+B10"));
        assert_eq!(v.undo.len(), 1);
        assert_eq!(v.range(), (9, 0, 9, 1));
    }

    #[test]
    fn a_copy_and_the_menus_other_choices() {
        let mut v = view();
        put(&mut v, "A1", Cell::number(2.0));
        put(&mut v, "B1", Cell::formula("A1*10"));
        v.border_drop((0, 0, 0, 1), at("A3"), DropChoice::Copy)
            .unwrap();
        assert_eq!(formula(&v, "B3").as_deref(), Some("A3*10"));
        assert_eq!(value(&v, "A1"), CellValue::Number(2.0), "a copy leaves it");
        v.border_drop((0, 0, 0, 1), at("A5"), DropChoice::CopyValues)
            .unwrap();
        assert_eq!(formula(&v, "B5"), None);
        assert_eq!(value(&v, "B5"), CellValue::Number(20.0));
        v.border_drop((0, 0, 0, 0), at("D1"), DropChoice::Link)
            .unwrap();
        assert_eq!(formula(&v, "D1").as_deref(), Some("$A$1"));
        assert_eq!(
            v.border_drop((0, 0, 0, 0), at("E1"), DropChoice::Cancel),
            Ok(false)
        );
        assert_eq!(value(&v, "E1"), CellValue::Empty);
        assert_eq!(v.undo.len(), 3);
    }

    #[test]
    fn data_under_the_drop_is_noticed_outside_the_source_only() {
        let mut v = view();
        put(&mut v, "A1", Cell::number(1.0));
        put(&mut v, "A2", Cell::number(2.0));
        put(&mut v, "C5", Cell::text("x"));
        // Moving A1:A2 down one row lands on A2, its own cell.
        assert!(!v.drop_hits_data((0, 0, 1, 0), (1, 0, 2, 0)));
        assert!(v.drop_hits_data((0, 0, 1, 0), (4, 2, 5, 2)));
        assert!(!v.drop_hits_data((0, 0, 1, 0), (6, 2, 7, 2)));
    }

    #[test]
    fn a_protected_sheet_refuses() {
        let mut v = view();
        put(&mut v, "A1", Cell::number(1.0));
        v.pkg.workbook.sheets[0].set_protected(true);
        assert!(
            v.border_drop((0, 0, 0, 0), at("B1"), DropChoice::Move)
                .is_err()
        );
        assert_eq!(value(&v, "A1"), CellValue::Number(1.0));
    }
}
