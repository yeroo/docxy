//! The host-agnostic spreadsheet session: everything the wasm ABI exposes,
//! written as plain Rust so it can be unit-tested natively
//! (`cargo test -p gridwasm`). Mirrors `docxwasm::bridge` in shape.

use gridcore::engine::{Engine, PART_OF_ARRAY, cell_to_value, eval_formula_at};
use gridcore::format::{FormatPatch, FormatValue, apply_patch_to_xf, xf_format_fields};
use gridcore::formula::Value;
use gridcore::frame::{Agg, Frame, pivot, pivot_spec_from_names, pivot_table_strings, range_stats};
use gridcore::sheet::{
    Align, Cell, CellValue, DefinedName, DrawingKind, MAX_COLS, MAX_ROWS, Sheet, Styles, cell_name,
    fmt_general, format_with, parse_cell_name, parse_col, parse_range_name, sheet_to_csv,
};
use gridcore::xlsx::{SheetPackage, SpreadsheetKind, load_xlsx, save_xlsx, save_xlsx_as};

use crate::json;

/// Bytes of a fresh empty workbook (the host's empty-file create flow).
pub fn new_workbook() -> Vec<u8> {
    save_xlsx(&gridcore::xlsx::new_xlsx())
}

/// One undoable action: cell states before/after, per address.
struct UndoGroup {
    sheet: usize,
    changes: Vec<(u32, u32, Option<Cell>, Option<Cell>)>,
    /// A restyle ([`Session::apply_styles`]): undo/redo put back only each
    /// cell's style, so a spill the cells belong to stays whole.
    styles_only: bool,
}

/// The `(row, col, style)` a restyle group puts back: the style of each
/// change's `before` or `after` cell (picked by `side`), default if absent.
fn group_styles(
    group: &UndoGroup,
    side: impl Fn(&(u32, u32, Option<Cell>, Option<Cell>)) -> &Option<Cell>,
) -> Vec<(u32, u32, u32)> {
    group
        .changes
        .iter()
        .map(|ch| (ch.0, ch.1, side(ch).as_ref().map_or(0, |cl| cl.style)))
        .collect()
}

/// The `(row, col, cell)` an undo or redo puts back: each change's `before`
/// or `after` cell (picked by `side`), blank if absent.
fn group_cells<'a>(
    changes: impl Iterator<Item = &'a (u32, u32, Option<Cell>, Option<Cell>)>,
    side: impl Fn(&(u32, u32, Option<Cell>, Option<Cell>)) -> &Option<Cell>,
) -> Vec<(u32, u32, Cell)> {
    changes
        .map(|ch| (ch.0, ch.1, side(ch).clone().unwrap_or_default()))
        .collect()
}

/// Sheets + defined names — snapshotted around structural edits whose inverse
/// is not expressible as per-cell changes (Task 3 uses this).
#[derive(Clone)]
struct WbSnapshot {
    sheets: Vec<Sheet>,
    names: Vec<DefinedName>,
}

enum UndoAction {
    Cells(UndoGroup),
    Structural {
        before: WbSnapshot,
        after: WbSnapshot,
    },
    /// A sheet-add: its inverse is the real `SheetPackage::remove_sheet`
    /// (part + content-type override + workbook rel + workbook.xml entry),
    /// not a `WbSnapshot` restore. A `Structural` snapshot only rolls back
    /// `workbook.sheets`, leaving the minted worksheet part and its
    /// `<sheet>` element in `xl/workbook.xml` behind; that count mismatch
    /// makes `patch_sheet_names` bail on save (see
    /// `gridcore::xlsx::patch_sheet_names`), so an add→undo→save round trip
    /// would resurrect the "undone" sheet empty and silently drop any
    /// rename saved in between.
    SheetAdd {
        idx: usize,
        name: String,
    },
}

/// A sheet's full content (cells, formulas, drawings, name) plus its
/// comments, captured by `ctl_sheet_remove` just before
/// `SheetPackage::remove_sheet` destroys them — the bridge-level stash that
/// makes `sheet.remove`'s declared `inverse` (the internal
/// `sheet.restore-removed` verb) a genuine, lossless reversal instead of the
/// earlier "recreate an empty sheet by the same name" placeholder. Single
/// slot: a second `sheet.remove` overwrites it, and a successful restore
/// takes (clears) it.
struct RemovedSheetStash {
    name: String,
    sheet: Sheet,
    /// Every comment (threaded or legacy) anchored on the removed sheet,
    /// captured via `SheetPackage::comments()` *before* removal — comment
    /// data lives in package parts outside `Sheet`, so it isn't part of the
    /// `sheet` snapshot above and would otherwise be silently dropped.
    comments: Vec<gridcore::comments::Comment>,
    /// Every defined name SCOPED to the removed sheet (`scope ==
    /// Some(idx)`) — `SheetPackage::remove_sheet` drops these via a
    /// `defined_names.retain(|d| d.scope != Some(idx))` (gridcore/src/
    /// xlsx.rs), and they live on `Workbook::defined_names`, not `Sheet`,
    /// so they too would otherwise be silently dropped. Replayed on
    /// restore with `scope` re-pointed at the restored sheet's NEW index.
    defined_names: Vec<DefinedName>,
    /// Whether the removed sheet was the ACTIVE one at the moment of
    /// removal — mirrors `sheet.remove`'s own viewport-reset rule, in
    /// reverse, on restore (see `ctl_sheet_restore_removed`).
    removed_active: bool,
    /// Every pivot whose output lived on the removed sheet (`.sheet ==
    /// removed_idx` — the SAME filter `SheetPackage::remove_sheet`'s own
    /// cascade uses), captured before that cascade drops their OPC parts
    /// and `workbook.pivots` entries. Wave-3 Task 4 review finding: Task
    /// 3's `remove_sheet` cascade (added so `pivot.create`'s inverse is
    /// "both or neither") silently broke this stash's own lossless-restore
    /// contract for ANY workbook with pivots — a `sheet.remove` +
    /// `sheet.restore-removed` round trip brought the sheet back WITHOUT
    /// its pivot. Reconstituted on restore via `ctl_reregister_pivot`,
    /// `.sheet` re-pointed at the restored sheet's NEW index.
    pivots: Vec<gridcore::pivot::Pivot>,
}

/// A live editing session over one `.xlsx`.
pub struct Session {
    /// Whole package retained — save regenerates only modeled cell data and
    /// preserves every other part byte-for-byte.
    pkg: SheetPackage,
    engine: Engine,
    /// Active sheet index (what the webview is looking at).
    active: usize,
    /// Active cell (row, col) and the optional selection anchor it extends from.
    cur: (u32, u32),
    anchor: Option<(u32, u32)>,
    /// Last requested viewport: (top, left, nrows, ncols).
    window: (u32, u32, u32, u32),
    dirty: bool,
    /// One-shot status/error line for the next view (formula errors etc.).
    err: Option<String>,
    undo: Vec<UndoAction>,
    redo: Vec<UndoAction>,
    /// The most recently `sheet.remove`d sheet, if any — see
    /// [`RemovedSheetStash`].
    removed_sheet_stash: Option<RemovedSheetStash>,
    /// Incremented every time a REAL undo group lands on `undo` (i.e. a
    /// genuine mutation was committed) — never touched by `do_undo`/
    /// `do_redo`. The webview reads this from `view_json` and compares it
    /// before/after a `dispatch` call to decide whether to tell the host
    /// "this edit happened" (dirty dot, undo-stack entry), replacing a
    /// verb-name regex that couldn't tell a no-op `autosum`/`cut`/`paste`
    /// (nothing actually changed) from one that mutated.
    edits: u64,
    /// What this session's last `copy`/`cut` put on the clipboard.
    clip: Option<GridClip>,
}

/// A clipboard text as [`GridClip::text`] keeps it and a `paste` compares it:
/// CRLF read as LF and trailing line breaks dropped, the same on both sides.
/// A copy whose last row is empty ends in `\n`, and the host may hand the
/// text back with or without it.
fn clip_key(text: &str) -> String {
    text.replace("\r\n", "\n")
        .trim_end_matches('\n')
        .to_string()
}

/// A `copy`/`cut`'s cells as they were (formats, formulas, quote prefixes
/// intact), where they came from, and the TSV handed to the host. A `paste`
/// of exactly that text pastes these cells, as xlsxy's and the suite's own
/// clipboards do: the TSV carries no formats, so reading it back as typed
/// would add or drop an apostrophe between a Text and a General cell. Any
/// other text is read as typed.
#[derive(Clone, Debug)]
struct GridClip {
    cells: Vec<Vec<Option<Cell>>>,
    from: (u32, u32),
    /// A cut: its source is already cleared, and its formulas keep their
    /// references when pasted (once; later pastes are copies, as in xlsxy).
    cut: bool,
    text: String,
}

impl Session {
    pub fn open(bytes: &[u8]) -> Option<Session> {
        let mut pkg = load_xlsx(bytes).ok()?;
        let mut engine = Engine::new(&pkg.workbook);
        engine.recalc_all(&mut pkg.workbook);
        Some(Session {
            pkg,
            engine,
            active: 0,
            cur: (0, 0),
            anchor: None,
            window: (0, 0, 60, 20),
            dirty: false,
            err: None,
            undo: Vec::new(),
            redo: Vec::new(),
            removed_sheet_stash: None,
            edits: 0,
            clip: None,
        })
    }

    /// Apply one tab-delimited command. Returns `Some(text)` when the host
    /// should copy `text` to the OS clipboard. (Commands grow over Tasks 2–4.)
    pub fn dispatch(&mut self, cmd: &str) -> Option<String> {
        let mut it = cmd.splitn(2, '\t');
        let op = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("");
        match op {
            "view" => {
                let p: Vec<&str> = rest.split('\t').collect();
                if p.len() == 5 {
                    let sheet: usize = p[0].parse().unwrap_or(0);
                    if sheet < self.pkg.workbook.sheets.len() {
                        self.active = sheet;
                    }
                    self.window = (
                        p[1].parse().unwrap_or(0),
                        p[2].parse().unwrap_or(0),
                        p[3].parse().unwrap_or(60).max(1),
                        p[4].parse().unwrap_or(20).max(1),
                    );
                }
            }
            "clock" => {
                if let Ok(serial) = rest.parse::<f64>() {
                    self.engine.clock = Some(serial);
                }
            }
            "select" => {
                let p: Vec<&str> = rest.split('\t').collect();
                if p.len() >= 2 {
                    let r = p[0].parse().unwrap_or(0);
                    let c = p[1].parse().unwrap_or(0);
                    self.cur = (r, c);
                    self.anchor = if p.len() == 4 {
                        Some((p[2].parse().unwrap_or(r), p[3].parse().unwrap_or(c)))
                    } else {
                        None
                    };
                }
            }
            "set" => {
                let p: Vec<&str> = rest.splitn(3, '\t').collect();
                if p.len() == 3 {
                    let (r, c): (u32, u32) = (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
                    if r >= MAX_ROWS || c >= MAX_COLS {
                        return None;
                    }
                    let text = p[2];
                    let wb = &self.pkg.workbook;
                    // A Text cell stores `=…` as text: nothing to validate.
                    if let Some(body) = gridcore::entry::typed_formula(wb, self.active, r, c, text)
                    {
                        if let Err(e) = Engine::validate(body) {
                            self.err = Some(format!("formula error: {e}"));
                            return None;
                        }
                    }
                    let today = self.engine.clock;
                    let wb = &mut self.pkg.workbook;
                    match gridcore::entry::entry_cell(wb, self.active, r, c, text, today) {
                        Ok(cell) => {
                            self.apply(vec![(r, c, cell)]);
                        }
                        Err(e) => {
                            self.err = Some(e.to_string());
                            return None;
                        }
                    }
                }
            }
            "clear" => {
                let p: Vec<&str> = rest.split('\t').collect();
                if p.len() == 4 {
                    let (r1, c1): (u32, u32) =
                        (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
                    let (r2, c2): (u32, u32) =
                        (p[2].parse().unwrap_or(0), p[3].parse().unwrap_or(0));
                    let changes = self.clear_changes((r1, c1, r2, c2));
                    self.apply(changes);
                }
            }
            "copy" => return Some(self.record_clip(false)),
            "cut" => {
                let (ar, ac) = self.anchor.unwrap_or(self.cur);
                let (r1, r2) = (self.cur.0.min(ar), self.cur.0.max(ar));
                let (c1, c2) = (self.cur.1.min(ac), self.cur.1.max(ac));
                // A cut of part of an array is refused whole, as Excel
                // refuses it: nothing is copied or cleared.
                let clears = self.clear_changes((r1, c1, r2, c2));
                if self
                    .engine
                    .refuses(&self.pkg.workbook, self.active, &clears)
                {
                    self.err = Some(PART_OF_ARRAY.to_string());
                    return None;
                }
                // The cells are recorded before the clear below empties them.
                let tsv = self.record_clip(true);
                // reuse the clear path as one undo group
                let cmd = format!("clear\t{r1}\t{c1}\t{r2}\t{c2}");
                self.dispatch(&cmd);
                return Some(tsv);
            }
            "paste" => {
                // rest = "<r>\t<c>\t<tsv...>" — split only twice, TSV keeps its tabs.
                let p: Vec<&str> = rest.splitn(3, '\t').collect();
                if p.len() == 3 {
                    let (r0, c0): (u32, u32) =
                        (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
                    if self.paste_own_clip(r0, c0, p[2]) {
                        return None;
                    }
                    // Cap the paste so a hostile/huge clipboard can't lock the
                    // UI in per-cell recalcs (mirrors the TUI's external-paste
                    // guard in xlsxy::main::paste).
                    const MAX_PASTE_CELLS: usize = 100_000;
                    let mut changes = Vec::new();
                    let mut truncated = false;
                    let ctx = gridcore::entry::entry_ctx(&self.pkg.workbook, self.engine.clock);
                    // Excel-on-Windows clipboards are CRLF and end with a
                    // trailing newline; strip both so a naive split doesn't
                    // (a) leave a stray \r on the last field of every row and
                    // (b) manufacture a phantom empty row past the paste that
                    // would clear whatever was already there.
                    'outer: for (dr, line) in p[2].trim_end_matches('\n').split('\n').enumerate() {
                        for (dc, text) in line.trim_end_matches('\r').split('\t').enumerate() {
                            if changes.len() >= MAX_PASTE_CELLS {
                                truncated = true;
                                break 'outer;
                            }
                            let (r, c) = (r0 + dr as u32, c0 + dc as u32);
                            if r >= MAX_ROWS || c >= MAX_COLS {
                                continue;
                            }
                            let style = self.pkg.workbook.sheets[self.active]
                                .cell(r, c)
                                .map(|x| x.style)
                                .unwrap_or(0);
                            // Read as typed into the target: a leading `'`
                            // is quote-prefixed text (what copy writes for
                            // one), a date brings its format.
                            let styles = &mut self.pkg.workbook.styles;
                            let mut cell = gridcore::entry::paste_cell(styles, style, text, &ctx);
                            // A pasted `=…` that doesn't parse would freeze as
                            // an unsupported cell (never evaluates, renders
                            // blank); demote it to literal text instead, same
                            // as the TUI's external paste.
                            if let Some(f) = &cell.formula {
                                if Engine::validate(f).is_err() {
                                    cell = Cell {
                                        value: CellValue::Text(text.to_string()),
                                        style: cell.style,
                                        ..Cell::default()
                                    };
                                }
                            }
                            changes.push((r, c, cell));
                        }
                    }
                    if self.apply(changes) && truncated {
                        self.err = Some(format!("Pasted (clipped to {MAX_PASTE_CELLS} cells)"));
                    }
                }
            }
            "fmt" => {
                // `rest` is "<key>\t<value>" — split only once so a numFmt
                // value keeps any inner tabs/chars intact.
                let mut fp = rest.splitn(2, '\t');
                let key = fp.next().unwrap_or("");
                let value = fp.next().unwrap_or("");
                self.apply_fmt(key, value);
            }
            "decimals" => {
                if let Ok(delta) = rest.trim().parse::<i32>() {
                    self.apply_decimals(delta);
                }
            }
            "autosum" => self.do_autosum(),
            "undo" => self.do_undo(),
            "redo" => self.do_redo(),
            "insrow" | "delrow" | "inscol" | "delcol" => {
                let p: Vec<&str> = rest.split('\t').collect();
                if p.len() == 2 {
                    let at: u32 = p[0].parse().unwrap_or(0);
                    let n: u32 = p[1].parse().unwrap_or(1).max(1);
                    let idx = self.active;
                    match op {
                        "insrow" => {
                            self.structural(|wb| gridcore::edit::insert_rows(wb, idx, at, n))
                        }
                        "delrow" => {
                            self.structural(|wb| gridcore::edit::delete_rows(wb, idx, at, n))
                        }
                        "inscol" => {
                            self.structural(|wb| gridcore::edit::insert_cols(wb, idx, at, n))
                        }
                        _ => self.structural(|wb| gridcore::edit::delete_cols(wb, idx, at, n)),
                    }
                }
            }
            "sheet" => {
                let p: Vec<&str> = rest.splitn(2, '\t').collect();
                match (p.first().copied(), p.get(1).copied()) {
                    (Some("switch"), Some(i)) => {
                        let i: usize = i.parse().unwrap_or(0);
                        if i < self.pkg.workbook.sheets.len() {
                            self.active = i;
                            self.cur = (0, 0);
                            self.anchor = None;
                        }
                    }
                    (Some("add"), Some(name)) if !name.is_empty() => {
                        // add_sheet operates on the whole SheetPackage (parts,
                        // not just Workbook), so its undo can't be a
                        // `Structural` before/after snapshot — that only
                        // rolls back `workbook.sheets`, leaving the minted
                        // worksheet part and its `<sheet>` entry in
                        // xl/workbook.xml behind (see the doc comment on
                        // `UndoAction::SheetAdd`). Nor can undo/redo simply
                        // be cleared here — that would desync the host's
                        // undo stack, since it registers one undo step per
                        // mutating command and an engine no-op undo would
                        // walk it back to a false-clean state. `remove_sheet`
                        // is the real inverse: it drops the part, the
                        // content-type override, the workbook rel, and the
                        // workbook.xml entry together.
                        let idx = self.pkg.add_sheet(name);
                        self.rebuild_engine();
                        self.undo.push(UndoAction::SheetAdd {
                            idx,
                            name: name.to_string(),
                        });
                        // A third genuine undo-group chokepoint alongside
                        // `apply()`/`structural()`: this push happens
                        // directly (see the comment above — a `SheetAdd`'s
                        // inverse isn't expressible as either of those), so
                        // it needs its own `edits` increment or the webview
                        // would stop posting `{type:'edit'}` for "+ add
                        // sheet" once the MUTATING regex it used to match
                        // against is gone.
                        self.edits += 1;
                        self.redo.clear();
                        self.active = idx;
                        self.cur = (0, 0);
                        self.anchor = None;
                        self.dirty = true;
                    }
                    (Some("rename"), Some(rest2)) => {
                        if let Some((i, name)) = rest2.split_once('\t') {
                            let i: usize = i.parse().unwrap_or(usize::MAX);
                            if i < self.pkg.workbook.sheets.len() && !name.is_empty() {
                                let n = name.to_string();
                                self.structural(|wb| gridcore::edit::rename_sheet(wb, i, &n));
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    /// Undo/redo snapshots of `keys` on sheet `sheet_idx` as it is now
    /// ([`gridcore::sheet::snapshot_cells`]): spill output of a live anchor
    /// is a blank the anchor re-spills over.
    fn snapshot(&self, sheet_idx: usize, keys: &[(u32, u32)]) -> Vec<Option<Cell>> {
        let wb = &self.pkg.workbook;
        let frozen = |r, c| self.engine.is_frozen(wb, (sheet_idx, r, c));
        gridcore::sheet::snapshot_cells(&wb.sheets[sheet_idx], keys, frozen)
    }

    /// Apply cell changes as one undo group, through the engine. Refused
    /// whole when they would change part of an array ([`Engine::refuses`]):
    /// nothing is written, no undo group is recorded, the session stays
    /// clean and the next view says why. False then.
    fn apply(&mut self, changes: Vec<(u32, u32, Cell)>) -> bool {
        if self
            .engine
            .refuses(&self.pkg.workbook, self.active, &changes)
        {
            self.err = Some(PART_OF_ARRAY.to_string());
            return false;
        }
        let keys = changes.iter().map(|&(r, c, _)| (r, c)).collect();
        self.record(keys, |s| {
            let sheet_idx = s.active;
            s.engine.set_cells(&mut s.pkg.workbook, sheet_idx, changes);
        });
        true
    }

    /// The blanks a clear of `(r1, c1, r2, c2)` on the active sheet writes:
    /// each non-blank cell's content goes, its style stays.
    fn clear_changes(&self, (r1, c1, r2, c2): (u32, u32, u32, u32)) -> Vec<(u32, u32, Cell)> {
        let sheet = &self.pkg.workbook.sheets[self.active];
        let mut changes = Vec::new();
        for r in r1..=r2 {
            for c in c1..=c2 {
                if let Some(cell) = sheet.cell(r, c) {
                    if !cell.is_blank() {
                        let mut blank = Cell::default();
                        blank.style = cell.style;
                        changes.push((r, c, blank));
                    }
                }
            }
        }
        changes
    }

    /// Run `write`, which edits the cells `keys` names on the active sheet,
    /// as one undo group. Every cell's before is taken ahead of the first
    /// write and its after once the last is done, so the group is one
    /// snapshot of its cells, as [`Engine::restore_cells`] needs: taken cell
    /// by cell, a spill anchor's after could still claim a cell a later
    /// write blocked it with.
    fn record(&mut self, keys: Vec<(u32, u32)>, write: impl FnOnce(&mut Self)) {
        if keys.is_empty() {
            return;
        }
        let sheet_idx = self.active;
        // An edit can take what only its undo puts back from outside the
        // keys: overwriting a frozen anchor clears its cached block, and
        // typing into the block drops the anchor's extent. The group
        // records the anchor and its block too
        // ([`gridcore::sheet::frozen_spill_keys`]).
        let keys = {
            let wb = &self.pkg.workbook;
            let frozen = |r, c| self.engine.is_frozen(wb, (sheet_idx, r, c));
            gridcore::sheet::frozen_spill_keys(&wb.sheets[sheet_idx], &keys, frozen)
        };
        // Spill output of a live anchor is snapshotted as the blank its
        // anchor re-spills over ([`Session::snapshot`]), whether or not the
        // anchor is in the group.
        let snapshot = |s: &Self| s.snapshot(sheet_idx, &keys);
        let before = snapshot(self);
        write(self);
        let after = snapshot(self);
        let group = UndoGroup {
            sheet: sheet_idx,
            changes: keys
                .iter()
                .zip(before.into_iter().zip(after))
                .map(|(&(r, c), (b, a))| (r, c, b, a))
                .collect(),
            styles_only: false,
        };
        self.undo.push(UndoAction::Cells(group));
        self.edits += 1;
        self.redo.clear();
        self.dirty = true;
    }

    /// Restyle cells on sheet `sheet_idx` — `(row, col, style index)` — as
    /// one undo group. Only styles change ([`Engine::set_styles`]): a value,
    /// formula or spill is never re-entered, so formatting a spilled block
    /// keeps the spill.
    fn apply_styles(&mut self, sheet_idx: usize, styles: Vec<(u32, u32, u32)>) {
        if styles.is_empty() {
            return;
        }
        let before: Vec<Option<Cell>> = {
            let sheet = &self.pkg.workbook.sheets[sheet_idx];
            styles
                .iter()
                .map(|&(r, c, _)| sheet.cell(r, c).cloned())
                .collect()
        };
        self.engine
            .set_styles(&mut self.pkg.workbook, sheet_idx, &styles);
        let sheet = &self.pkg.workbook.sheets[sheet_idx];
        let changes = styles
            .iter()
            .zip(before)
            .map(|(&(r, c, _), before)| (r, c, before, sheet.cell(r, c).cloned()))
            .collect();
        self.undo.push(UndoAction::Cells(UndoGroup {
            sheet: sheet_idx,
            changes,
            styles_only: true,
        }));
        self.edits += 1;
        self.redo.clear();
        self.dirty = true;
    }

    fn do_undo(&mut self) {
        match self.undo.pop() {
            Some(UndoAction::Cells(group)) => {
                self.active = group.sheet.min(self.pkg.workbook.sheets.len() - 1);
                if group.styles_only {
                    let styles = group_styles(&group, |ch| &ch.2);
                    self.engine
                        .set_styles(&mut self.pkg.workbook, group.sheet, &styles);
                } else {
                    let cells = group_cells(group.changes.iter().rev(), |ch| &ch.2);
                    self.engine
                        .restore_cells(&mut self.pkg.workbook, group.sheet, &cells);
                }
                self.redo.push(UndoAction::Cells(group));
                self.dirty = true;
            }
            Some(UndoAction::Structural { before, after }) => {
                self.restore(&before);
                self.redo.push(UndoAction::Structural { before, after });
            }
            Some(UndoAction::SheetAdd { idx, name }) => {
                self.pkg.remove_sheet(idx);
                self.rebuild_engine();
                // Sheet count shrinks — keep the active index in bounds,
                // same clamp as the Cells case above.
                self.active = self.active.min(self.pkg.workbook.sheets.len() - 1);
                self.dirty = true;
                self.redo.push(UndoAction::SheetAdd { idx, name });
            }
            None => {}
        }
    }

    fn do_redo(&mut self) {
        match self.redo.pop() {
            Some(UndoAction::Cells(group)) => {
                self.active = group.sheet.min(self.pkg.workbook.sheets.len() - 1);
                if group.styles_only {
                    let styles = group_styles(&group, |ch| &ch.3);
                    self.engine
                        .set_styles(&mut self.pkg.workbook, group.sheet, &styles);
                } else {
                    let cells = group_cells(group.changes.iter(), |ch| &ch.3);
                    self.engine
                        .restore_cells(&mut self.pkg.workbook, group.sheet, &cells);
                }
                self.undo.push(UndoAction::Cells(group));
                self.dirty = true;
            }
            Some(UndoAction::Structural { before, after }) => {
                self.restore(&after);
                self.undo.push(UndoAction::Structural { before, after });
            }
            Some(UndoAction::SheetAdd { idx: _, name }) => {
                let new_idx = self.pkg.add_sheet(&name);
                self.rebuild_engine();
                self.active = new_idx;
                self.dirty = true;
                self.undo.push(UndoAction::SheetAdd { idx: new_idx, name });
            }
            None => {}
        }
    }

    /// Snapshot-run-snapshot for structural edits (row/col ops, renames):
    /// the inverse isn't per-cell, so undo restores the whole grid state.
    fn structural(&mut self, op: impl FnOnce(&mut gridcore::sheet::Workbook)) {
        let before = WbSnapshot {
            sheets: self.pkg.workbook.sheets.clone(),
            names: self.pkg.workbook.defined_names.clone(),
        };
        op(&mut self.pkg.workbook);
        self.rebuild_engine();
        let after = WbSnapshot {
            sheets: self.pkg.workbook.sheets.clone(),
            names: self.pkg.workbook.defined_names.clone(),
        };
        self.undo.push(UndoAction::Structural { before, after });
        self.edits += 1;
        self.redo.clear();
        self.dirty = true;
    }

    fn restore(&mut self, snap: &WbSnapshot) {
        self.pkg.workbook.sheets = snap.sheets.clone();
        self.pkg.workbook.defined_names = snap.names.clone();
        self.rebuild_engine();
        self.dirty = true;
    }

    /// Formulas changed wholesale — reparse the graph and refresh values.
    fn rebuild_engine(&mut self) {
        let clock = self.engine.clock;
        let seed = self.engine.seed;
        let mut engine = Engine::new(&self.pkg.workbook);
        engine.clock = clock;
        engine.seed = seed;
        engine.recalc_all(&mut self.pkg.workbook);
        self.engine = engine;
    }

    /// What the editor and formula bar start from (and a copy carries):
    /// [`gridcore::entry::seed_text`] — every digit of a number, a percent
    /// cell's `150%`, a quote prefix's apostrophe — as xlsxy and the suite
    /// seed theirs. grid.js commits the editor even when it is unchanged, so
    /// the cell is re-read: a number past 15 significant digits comes back
    /// rounded to 15, and an E-notation seed (`1E300`) in a General cell
    /// brings the scientific format `0.00E+00`.
    fn cell_seed(&self, row: u32, col: u32) -> String {
        let styles = &self.pkg.workbook.styles;
        match self.pkg.workbook.sheets[self.active].cell(row, col) {
            Some(cell) => gridcore::entry::seed_text(cell, &styles.xf(cell.style)),
            None => String::new(),
        }
    }

    /// Serialize the current workbook. Lossless: only modeled cell data is
    /// regenerated; every other part is preserved byte-for-byte.
    pub fn save(&mut self) -> Vec<u8> {
        self.save_as(None)
    }

    /// [`Self::save`] as a file of `kind`: the workbook part's content type
    /// follows it, and a macro-free kind leaves the VBA project and Excel 4.0
    /// macro sheets out of the bytes (the open workbook keeps them). `None`
    /// keeps the loaded type.
    pub fn save_as(&mut self, kind: Option<SpreadsheetKind>) -> Vec<u8> {
        let out = match kind {
            Some(kind) => save_xlsx_as(&self.pkg, kind),
            None => save_xlsx(&self.pkg),
        };
        self.dirty = false;
        out
    }

    /// Record the selection as this session's clip and return its TSV.
    fn record_clip(&mut self, cut: bool) -> String {
        let text = self.selection_tsv();
        let (ar, ac) = self.anchor.unwrap_or(self.cur);
        let (r1, r2) = (self.cur.0.min(ar), self.cur.0.max(ar));
        let (c1, c2) = (self.cur.1.min(ac), self.cur.1.max(ac));
        let sheet = &self.pkg.workbook.sheets[self.active];
        let cells = (r1..=r2)
            .map(|r| (c1..=c2).map(|c| sheet.cell(r, c).cloned()).collect())
            .collect();
        self.clip = Some(GridClip {
            cells,
            from: (r1, c1),
            cut,
            text: clip_key(&text),
        });
        text
    }

    /// Paste this session's own clip at `(r0, c0)` when `text` is what it
    /// put on the clipboard (the host may hand back CRLF and a trailing line
    /// break). A copy's formulas move their relative references with the
    /// paste; a cut's keep them. False when `text` is someone else's.
    fn paste_own_clip(&mut self, r0: u32, c0: u32, text: &str) -> bool {
        let text = clip_key(text);
        let Some(clip) = self.clip.clone().filter(|c| c.text == text) else {
            return false;
        };
        let (dr_all, dc_all) = (
            r0 as i64 - clip.from.0 as i64,
            c0 as i64 - clip.from.1 as i64,
        );
        // The block written from (r0, c0): rows and cells pushed off the
        // grid's edge are dropped, so rows may differ in length.
        let mut block = Vec::new();
        let mut keys = Vec::new();
        for (dr, row) in clip.cells.iter().enumerate() {
            let r = r0 + dr as u32;
            if r >= MAX_ROWS {
                break;
            }
            let mut out = Vec::new();
            for (dc, cell) in row.iter().enumerate() {
                let c = c0 + dc as u32;
                if c >= MAX_COLS {
                    break;
                }
                let mut cell = cell.clone().unwrap_or_default();
                if !clip.cut && (dr_all, dc_all) != (0, 0) {
                    if let Some(f) = &cell.formula {
                        if let Some(t) = gridcore::formula::translate_formula(f, dr_all, dc_all) {
                            cell.formula = Some(t);
                        }
                    }
                }
                keys.push((r, c));
                out.push(cell);
            }
            block.push(out);
        }
        // Refused whole when it would change part of an array; still our
        // clip, so it is not pasted again as text.
        if self
            .engine
            .refuses_paste(&self.pkg.workbook, self.active, (r0, c0), &block, &[])
        {
            self.err = Some(PART_OF_ARRAY.to_string());
            return true;
        }
        // Through `Engine::paste_block`, as one undo group: a pasted spilling
        // array still spills, and an array block pasted back in place keeps
        // its block.
        self.record(keys, |s| {
            let sheet_idx = s.active;
            s.engine
                .paste_block(&mut s.pkg.workbook, sheet_idx, (r0, c0), &block);
        });
        if clip.cut {
            if let Some(c) = self.clip.as_mut() {
                c.cut = false;
            }
        }
        true
    }

    /// The selection as TSV of each cell's editor seed
    /// ([`Session::cell_seed`]: formulas as `=...`, a quote prefix's `'`),
    /// rows by `\n`, cells by `\t` — round-trips through `paste`.
    fn selection_tsv(&self) -> String {
        let (ar, ac) = self.anchor.unwrap_or(self.cur);
        let (r1, r2) = (self.cur.0.min(ar), self.cur.0.max(ar));
        let (c1, c2) = (self.cur.1.min(ac), self.cur.1.max(ac));
        let mut rows = Vec::new();
        for r in r1..=r2 {
            let mut cells = Vec::new();
            for c in c1..=c2 {
                // The editor's text: a quote-prefixed `'007` copies with its
                // apostrophe, so paste gives back text, not the number 7.
                cells.push(self.cell_seed(r, c));
            }
            rows.push(cells.join("\t"));
        }
        rows.join("\n")
    }

    /// Apply `patch` over every cell in the CURRENT SELECTION rectangle
    /// (`self.cur`/`self.anchor`) as one undo group. The exact
    /// snapshot→apply pattern `ctl_cell_format` uses (see that method):
    /// snapshot each cell's existing style first (so the loop below never
    /// observes an already-patched style from earlier in the same range),
    /// compute the patched `Xf`, intern it, and hand the whole batch to
    /// `apply_styles()` — one `UndoGroup`, one `dispatch("undo")` to revert.
    fn apply_format_to_selection(&mut self, patch: &FormatPatch) {
        let (ar, ac) = self.anchor.unwrap_or(self.cur);
        let (r1, r2) = (self.cur.0.min(ar), self.cur.0.max(ar));
        let (c1, c2) = (self.cur.1.min(ac), self.cur.1.max(ac));
        // Same cap `ctl_cell_format` enforces, and for the same reason: a
        // Select-All + a toolbar format click can span a huge used range,
        // and this materializes a `Cell` (plus an undo snapshot entry) per
        // coordinate — unbounded, that's an unbounded UI freeze. Surfaced
        // exactly like any other one-shot dispatch error: `self.err`, no
        // partial apply.
        if let Err(e) = gridcore::format::check_format_range_cap(r1, c1, r2, c2) {
            self.err = Some(e);
            return;
        }
        let sheet_idx = self.active;
        let snapshot: Vec<(u32, u32, u32)> = {
            let sheet = &self.pkg.workbook.sheets[sheet_idx];
            let mut v = Vec::new();
            for r in r1..=r2 {
                for c in c1..=c2 {
                    v.push((r, c, sheet.cell(r, c).map_or(0, |cl| cl.style)));
                }
            }
            v
        };
        let mut styles = Vec::with_capacity(snapshot.len());
        for (r, c, cur_style) in snapshot {
            let base_xf = self.pkg.workbook.styles.xf(cur_style);
            let new_xf = apply_patch_to_xf(&base_xf, patch);
            let idx = self.pkg.workbook.styles.intern(new_xf);
            styles.push((r, c, idx));
        }
        // Only styles change: a spilled block stays spilled (#784).
        self.apply_styles(sheet_idx, styles);
    }

    /// `fmt\t<key>\t<value>` — the toolbar's one-key format verbs.
    /// `bold`/`italic` with value `"toggle"` read the ACTIVE cell's current
    /// state and apply the FLIPPED state to the whole selection (Excel
    /// semantics: the active cell decides the target, the selection
    /// receives it). The other keys just parse `value` as that key's
    /// [`FormatPatch`] field. An unrecognized key or a value `FormatPatch`
    /// rejects is silently ignored — no partial format, no undo group.
    fn apply_fmt(&mut self, key: &str, value: &str) {
        let pair = match key {
            "bold" | "italic" if value == "toggle" => {
                let (cr, cc) = self.cur;
                let active_style = self.pkg.workbook.sheets[self.active]
                    .cell(cr, cc)
                    .map(|cl| cl.style)
                    .unwrap_or(0);
                let xf = self.pkg.workbook.styles.xf(active_style);
                let current = if key == "bold" { xf.bold } else { xf.italic };
                let target = !current;
                (
                    key.to_string(),
                    (if target { "true" } else { "false" }).to_string(),
                )
            }
            "fontColor" | "fillColor" | "align" | "numFmt" => (key.to_string(), value.to_string()),
            _ => return,
        };
        if let Ok(patch) = FormatPatch::parse(&[pair]) {
            self.apply_format_to_selection(&patch);
        }
    }

    /// `decimals\t<±n>` — increase/decrease the decimal-place count of the
    /// ACTIVE cell's current number format (via [`gridcore::format::adjust_decimals`])
    /// and apply the resulting code to the whole selection.
    fn apply_decimals(&mut self, delta: i32) {
        let (cr, cc) = self.cur;
        let active_style = self.pkg.workbook.sheets[self.active]
            .cell(cr, cc)
            .map(|cl| cl.style)
            .unwrap_or(0);
        let xf = self.pkg.workbook.styles.xf(active_style);
        let current_code = xf.code.as_deref().unwrap_or("");
        let new_code = gridcore::format::adjust_decimals(current_code, delta);
        if let Ok(patch) = FormatPatch::parse(&[("numFmt".to_string(), new_code)]) {
            self.apply_format_to_selection(&patch);
        }
    }

    /// Whether `(r, c)` on the active sheet holds a numeric value — a
    /// literal number OR a formula's cached numeric result. Used only by
    /// `do_autosum`'s range guess.
    fn is_numeric_cell(&self, r: u32, c: u32) -> bool {
        self.pkg.workbook.sheets[self.active]
            .cell(r, c)
            .map(|cl| matches!(cl.value, CellValue::Number(_)))
            .unwrap_or(false)
    }

    /// `autosum` — classic Excel range guess: the contiguous run of numeric
    /// cells directly ABOVE the active cell; if there is none, the
    /// contiguous run directly to its LEFT; if neither, a no-op that
    /// surfaces `self.err` instead of writing `=SUM()` over nothing. Writes
    /// the active cell as `=SUM(<range>)`, one undo group (via `apply`).
    fn do_autosum(&mut self) {
        let (r0, c0) = self.cur;
        let mut top = None;
        if r0 > 0 {
            let mut r = r0 - 1;
            loop {
                if !self.is_numeric_cell(r, c0) {
                    break;
                }
                top = Some(r);
                if r == 0 {
                    break;
                }
                r -= 1;
            }
        }
        let range = if let Some(top_r) = top {
            Some(((top_r, c0), (r0 - 1, c0)))
        } else if c0 > 0 {
            let mut left = None;
            let mut c = c0 - 1;
            loop {
                if !self.is_numeric_cell(r0, c) {
                    break;
                }
                left = Some(c);
                if c == 0 {
                    break;
                }
                c -= 1;
            }
            left.map(|left_c| ((r0, left_c), (r0, c0 - 1)))
        } else {
            None
        };
        let Some(((r1, c1), (r2, c2))) = range else {
            self.err = Some("AutoSum: no numbers to sum".to_string());
            return;
        };
        let formula = format!("SUM({}:{})", cell_name(r1, c1), cell_name(r2, c2));
        let style = self.pkg.workbook.sheets[self.active]
            .cell(r0, c0)
            .map(|cl| cl.style)
            .unwrap_or(0);
        let mut cell = Cell::formula(&formula);
        cell.style = style;
        self.apply(vec![(r0, c0, cell)]);
    }

    /// Render the current viewport to the JSON the webview consumes. `copied`,
    /// when set, carries text the host should place on the OS clipboard (from
    /// a copy or cut command).
    pub fn view_json(&mut self, copied: Option<&str>) -> String {
        let wb = &self.pkg.workbook;
        let sh = &wb.sheets[self.active];
        let (top, left, nrows, ncols) = self.window;
        let (used_r, used_c) = sh.used_size();

        let mut out = String::with_capacity(4096);
        out.push_str("{\"sheets\":[");
        for (i, s) in wb.sheets.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json::push_str(&mut out, &s.name);
        }
        out.push_str("],\"active\":");
        out.push_str(&self.active.to_string());
        out.push_str(",\"dims\":{\"rows\":");
        out.push_str(&used_r.to_string());
        out.push_str(",\"cols\":");
        out.push_str(&used_c.to_string());
        out.push_str("},\"colw\":[");
        for (i, c) in (left..left.saturating_add(ncols)).enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"c\":");
            out.push_str(&c.to_string());
            out.push_str(",\"w\":");
            out.push_str(&format!("{:.2}", sh.col_width(c)));
            out.push('}');
        }
        out.push_str("],\"cells\":[");
        let mut first = true;
        for r in top..top.saturating_add(nrows) {
            for c in left..left.saturating_add(ncols) {
                let Some(cell) = sh.cell(r, c) else { continue };
                if cell.is_blank() {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                let xf = wb.styles.xf(cell.style);
                // A General number is fitted to its column in characters, as
                // Excel's General shows it (at most 11; the editor and a copy
                // keep every digit).
                let text = match cell.value {
                    CellValue::Number(n) if gridcore::entry::is_general(&xf) => {
                        let chars = sh.col_width(c).floor().max(1.0) as usize;
                        gridcore::sheet::fmt_general_cell(n, chars)
                    }
                    _ => format_with(&xf, &cell.value, wb.date1904),
                };
                out.push_str("{\"r\":");
                out.push_str(&r.to_string());
                out.push_str(",\"c\":");
                out.push_str(&c.to_string());
                out.push_str(",\"t\":");
                json::push_str(&mut out, &text);
                let align = match xf.align {
                    Align::Right => Some("r"),
                    Align::Center => Some("c"),
                    Align::Left => None,
                    Align::General => match cell.value {
                        CellValue::Number(_) | CellValue::Bool(_) => Some("r"),
                        _ => None,
                    },
                };
                if let Some(a) = align {
                    out.push_str(",\"a\":\"");
                    out.push_str(a);
                    out.push('"');
                }
                if xf.bold {
                    out.push_str(",\"b\":1");
                }
                if xf.italic {
                    out.push_str(",\"i\":1");
                }
                if let Some((r8, g8, b8)) = xf.color {
                    out.push_str(&format!(",\"col\":\"#{r8:02x}{g8:02x}{b8:02x}\""));
                }
                if let Some((r8, g8, b8)) = xf.fill {
                    out.push_str(&format!(",\"bg\":\"#{r8:02x}{g8:02x}{b8:02x}\""));
                }
                out.push('}');
            }
        }
        out.push_str("],\"sel\":{");
        let (ar, ac) = self.anchor.unwrap_or(self.cur);
        let (r1, r2) = (self.cur.0.min(ar), self.cur.0.max(ar));
        let (c1, c2) = (self.cur.1.min(ac), self.cur.1.max(ac));
        out.push_str(&format!("\"r\":{r1},\"c\":{c1},\"r2\":{r2},\"c2\":{c2}"));
        out.push_str("},\"cur\":{\"ref\":");
        json::push_str(&mut out, &cell_name(self.cur.0, self.cur.1));
        out.push_str(",\"src\":");
        let src = self.cell_seed(self.cur.0, self.cur.1);
        json::push_str(&mut out, &src);
        // The active cell's format, so the toolbar can show pressed state
        // (bold/italic buttons, the align group) without a separate round
        // trip. `align` is the wire letter, not the full word — 'g' (not
        // 'l'/'c'/'r') means General/no button lit, mirroring the `cells[]`
        // alignment mapping just above but WITHOUT that mapping's
        // value-dependent General fallback (the toolbar cares about the
        // cell's actual style, not how a General number happens to render).
        let active_style = self.pkg.workbook.sheets[self.active]
            .cell(self.cur.0, self.cur.1)
            .map(|cl| cl.style)
            .unwrap_or(0);
        let active_xf = self.pkg.workbook.styles.xf(active_style);
        out.push_str(",\"bold\":");
        out.push_str(if active_xf.bold { "true" } else { "false" });
        out.push_str(",\"italic\":");
        out.push_str(if active_xf.italic { "true" } else { "false" });
        out.push_str(",\"align\":\"");
        out.push_str(match active_xf.align {
            Align::Left => "l",
            Align::Center => "c",
            Align::Right => "r",
            Align::General => "g",
        });
        out.push('"');
        out.push_str("},\"dirty\":");
        out.push_str(if self.dirty { "true" } else { "false" });
        // The webview's `userCmd` compares this before/after a dispatch call
        // to decide whether to tell the host "an edit happened" — see the
        // `edits` field's doc comment on `Session`.
        out.push_str(",\"edits\":");
        out.push_str(&self.edits.to_string());
        if let Some(e) = self.err.take() {
            out.push_str(",\"err\":");
            json::push_str(&mut out, &e);
        }
        if let Some(t) = copied {
            out.push_str(",\"copied\":");
            json::push_str(&mut out, t);
        }
        out.push('}');
        out
    }
}

// -----------------------------------------------------------------------
// Agent control surface (`grid_ctl`)
// -----------------------------------------------------------------------
//
// Routes one control verb against this session's live workbook. The reply
// shape is byte-for-byte the same as xlsxy's control server
// (`xlsxy/src/control.rs`, the semantic contract this mirrors): the verb
// result object plus `"ok":true` on success, or `{"ok":false,"error":"…"}`
// on failure. `cell.set`/`range.clear` route through `dispatch`'s existing
// `"set"`/`"clear"` command paths — the very same ones interactive edits
// use — so an agent edit lands on the same undo stack, recalculates
// dependents, and marks the session dirty exactly like a keystroke.
//
// `wb.save`/`wb.reload`/`wb.open`/`wb.path` are HOST verbs (the host owns
// the file on disk in the VS Code extension model) and are not implemented
// here; `wb.info` gives the host the in-session shape it needs
// (`sheets`/`active`/`modified`) to compose its own `wb.path`-equivalent
// reply with URI info the host — not this crate — has.

/// The most cells one `sheet.read` returns (non-empty cells in the window);
/// larger reads set `truncated: true` so a client narrows the range.
const CTL_READ_CAP: usize = 5000;
/// The most matches one `find` returns.
const CTL_FIND_CAP: usize = 200;
/// The author stamped on a new comment when `author` is omitted. There is no
/// OS user to read in a wasm/webview host (unlike xlsxy's terminal, which
/// falls back to `$USER`/`$USERNAME`), so this is a fixed default.
const CTL_DEFAULT_COMMENT_AUTHOR: &str = "agent";

impl Session {
    /// Route one JSON control request (`{"verb":…,"args":{…}}`) and return the
    /// JSON reply. See the module note above for the reply envelope.
    pub fn ctl(&mut self, request_json: &str) -> String {
        let req = match json::Json::parse(request_json) {
            Ok(v) => v,
            Err(e) => return ctl_err(&format!("bad request: {e}")),
        };
        let verb = req.get_str("verb").unwrap_or("");
        let no_args = json::Json::Null;
        let args = req.get("args").unwrap_or(&no_args);
        let result = match verb {
            "sheet.list" => Ok(self.ctl_sheet_list()),
            "sheet.read" => self.ctl_sheet_read(args),
            "cell.get" => self.ctl_cell_get(args),
            "cell.set" => self.ctl_cell_set(args),
            "range.clear" => self.ctl_range_clear(args),
            "find" => self.ctl_find(args),
            "wb.recalc" => {
                self.engine.recalc_all(&mut self.pkg.workbook);
                // Mirrors xlsxy control.rs's `wb.recalc` -> `App::recalc_and_refresh`:
                // a plain engine recalc alone never touches pivot output —
                // `refresh_pivots` recomputes each supported pivot from its
                // (possibly just-edited) source range, then a follow-up
                // targeted recalc propagates any resulting cell changes to
                // formulas that read the pivot's output region. Skips
                // gracefully (no crash, `out.skipped` counts it) when a
                // pivot's source sheet has itself been removed — see
                // gridcore's `refreshing_a_pivots_output_after_its_source_
                // sheet_is_removed_skips_gracefully`.
                let outcome = gridcore::pivot::refresh_pivots(&mut self.pkg.workbook);
                if !outcome.changed.is_empty() {
                    self.engine
                        .recalc_from(&mut self.pkg.workbook, &outcome.changed);
                    self.dirty = true;
                }
                Ok("{\"recalculated\":true}".to_string())
            }
            "wb.info" => Ok(self.ctl_wb_info()),
            "comment.list" => Ok(self.ctl_comment_list()),
            "comment.add" => self.ctl_comment_add(args),
            "comment.remove" => self.ctl_comment_remove(args),
            // INTERNAL-ONLY: the host invokes this in response to
            // comment.add/comment.remove's declared `inverse`, through the same
            // ctl channel — but it must NOT appear in `wasmVerbs`, so an
            // external agent calling it through `CtlServer` still gets
            // "unknown verb 'comment.replace-thread'". See
            // `ctl_comment_replace_thread`'s doc comment.
            "comment.replace-thread" => self.ctl_comment_replace_thread(args),
            "wb.export-csv" => self.ctl_wb_export_csv(args),
            "sheet.pivot" => self.ctl_sheet_pivot(args),
            "formula.eval" => self.ctl_formula_eval(args),
            "sheet.stats" => self.ctl_sheet_stats(args),
            "chart.list" => Ok(self.ctl_chart_list()),
            "pivot.list" => Ok(self.ctl_pivot_list()),
            "pivot.create" => self.ctl_pivot_create(args),
            "range.set" => self.ctl_range_set(args),
            "sheet.import-csv" => self.ctl_sheet_import_csv(args),
            "wb.replace-all" => self.ctl_wb_replace_all(args),
            "sheet.add" => self.ctl_sheet_add(args),
            "sheet.remove" => self.ctl_sheet_remove(args),
            // INTERNAL-ONLY: the host invokes this in response to
            // `sheet.remove`'s declared `inverse`, through the same ctl
            // channel — but Task 7 must not expose it in `wasmVerbs`, so an
            // external agent calling it through `CtlServer` still gets
            // "unknown verb 'sheet.restore-removed'". See
            // `ctl_sheet_restore_removed`'s doc comment.
            "sheet.restore-removed" => self.ctl_sheet_restore_removed(args),
            "sheet.rename" => self.ctl_sheet_rename(args),
            "row.insert" => self.ctl_row_op(args, true),
            "row.delete" => self.ctl_row_op(args, false),
            "col.insert" => self.ctl_col_op(args, true),
            "col.delete" => self.ctl_col_op(args, false),
            "cell.format" => self.ctl_cell_format(args),
            "col.width" => self.ctl_col_width(args),
            other => Err(format!("unknown verb '{other}'")),
        };
        match result {
            Ok(body) => ctl_ok(body),
            Err(e) => ctl_err(&e),
        }
    }

    /// `{active, sheets:[{index, name, rows, cols}]}`
    fn ctl_sheet_list(&self) -> String {
        let mut out = String::from("{\"active\":");
        out.push_str(&self.active.to_string());
        out.push_str(",\"sheets\":[");
        for (i, s) in self.pkg.workbook.sheets.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let (rows, cols) = s.used_size();
            out.push_str("{\"index\":");
            out.push_str(&i.to_string());
            out.push_str(",\"name\":");
            json::push_str(&mut out, &s.name);
            out.push_str(",\"rows\":");
            out.push_str(&rows.to_string());
            out.push_str(",\"cols\":");
            out.push_str(&cols.to_string());
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// `{sheet?, range?}` -> `{sheet, name, rows, cols, cells:[…], truncated}`
    fn ctl_sheet_read(&self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let s = &self.pkg.workbook.sheets[si];
        let (used_r, used_c) = s.used_size();
        let (r1, c1, r2, c2) = match args.get_str("range") {
            Some(rg) => ctl_parse_range(rg)?,
            // Whole used range (empty sheet -> the single cell A1).
            None => (0, 0, used_r.saturating_sub(1), used_c.saturating_sub(1)),
        };
        let mut cells = String::new();
        let mut count = 0usize;
        let mut truncated = false;
        for (&(r, c), cell) in s.cells.range((r1, 0)..=(r2, u32::MAX)) {
            if c < c1 || c > c2 || cell.is_blank() {
                continue;
            }
            if count >= CTL_READ_CAP {
                truncated = true;
                break;
            }
            if count > 0 {
                cells.push(',');
            }
            cells.push_str(&self.ctl_cell_json(r, c, cell));
            count += 1;
        }
        let mut out = String::from("{\"sheet\":");
        out.push_str(&si.to_string());
        out.push_str(",\"name\":");
        json::push_str(&mut out, &s.name);
        out.push_str(",\"rows\":");
        out.push_str(&used_r.to_string());
        out.push_str(",\"cols\":");
        out.push_str(&used_c.to_string());
        out.push_str(",\"cells\":[");
        out.push_str(&cells);
        out.push_str("],\"truncated\":");
        out.push_str(if truncated { "true" } else { "false" });
        out.push('}');
        Ok(out)
    }

    /// `{ref, sheet?}` -> `{ref, row, col, value, formula?, text, format?}`.
    /// The only mirror verb whose cell JSON carries the additive `format`
    /// object (see [`Session::ctl_cell_json_with_format`]) — matches xlsxy
    /// control.rs's scoping decision exactly (`sheet.read`/`find`/`cell.set`
    /// stay format-less; see Task 3's report, "Concerns" #3).
    fn ctl_cell_get(&self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let (r, c) = ctl_ref_arg(args)?;
        let s = &self.pkg.workbook.sheets[si];
        Ok(match s.cell(r, c) {
            Some(cell) => self.ctl_cell_json_with_format(r, c, cell),
            None => {
                let mut out = String::from("{\"ref\":");
                json::push_str(&mut out, &cell_name(r, c));
                out.push_str(",\"row\":");
                out.push_str(&r.to_string());
                out.push_str(",\"col\":");
                out.push_str(&c.to_string());
                out.push_str(",\"value\":null,\"text\":\"\"}");
                out
            }
        })
    }

    /// `{ref, text, sheet?}` -> `{ref, value, text, …}`. Reuses `dispatch`'s
    /// `"set"` command path, so this shares the interactive edit's formula
    /// validation, undo group, and recalculation.
    fn ctl_cell_set(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let (r, c) = ctl_ref_arg(args)?;
        let text = args.get_str("text").ok_or("cell.set needs 'text'")?;
        let prev_active = self.active;
        self.active = si;
        // Clear any stale error a prior interactive edit left in `self.err`
        // (it's a one-shot channel normally drained by `view_json`), so
        // `take()` below reads only THIS ctl edit's own error, not a false
        // failure carried over from before.
        self.err = None;
        self.dispatch(&format!("set\t{r}\t{c}\t{text}"));
        self.active = prev_active;
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        let s = &self.pkg.workbook.sheets[si];
        Ok(match s.cell(r, c) {
            Some(cell) => self.ctl_cell_json(r, c, cell),
            None => {
                let mut out = String::from("{\"ref\":");
                json::push_str(&mut out, &cell_name(r, c));
                out.push('}');
                out
            }
        })
    }

    /// `{range, sheet?}` -> `{cleared}`. Reuses `dispatch`'s `"clear"`
    /// command path — one undo group for the whole range.
    fn ctl_range_clear(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let rg = args.get_str("range").ok_or("range.clear needs a 'range'")?;
        let (r1, c1, r2, c2) = ctl_parse_range(rg)?;
        let cleared = self.pkg.workbook.sheets[si]
            .cells
            .range((r1, 0)..=(r2, u32::MAX))
            .filter(|(pos, cell)| pos.1 >= c1 && pos.1 <= c2 && !cell.is_blank())
            .count();
        let prev_active = self.active;
        self.active = si;
        self.dispatch(&format!("clear\t{r1}\t{c1}\t{r2}\t{c2}"));
        self.active = prev_active;
        let mut out = String::from("{\"cleared\":");
        out.push_str(&cleared.to_string());
        out.push('}');
        Ok(out)
    }

    /// `{query, sheet?}` -> `{query, count, matches:[…]}`
    fn ctl_find(&self, args: &json::Json) -> Result<String, String> {
        let query = args.get_str("query").ok_or("find needs a 'query'")?;
        if query.is_empty() {
            return Err("empty query".into());
        }
        let needle = query.to_lowercase();
        // A `sheet` arg restricts the search; default is every sheet.
        let only: Option<usize> = match args.get("sheet") {
            Some(_) => Some(self.ctl_sheet_arg(args)?),
            None => None,
        };
        let mut matches = String::new();
        let mut count = 0usize;
        'outer: for (si, s) in self.pkg.workbook.sheets.iter().enumerate() {
            if only.is_some_and(|o| o != si) {
                continue;
            }
            for (&(r, c), cell) in &s.cells {
                let text_hit = self.ctl_cell_text(cell).to_lowercase().contains(&needle);
                let formula_hit = cell
                    .formula
                    .as_deref()
                    .is_some_and(|f| f.to_lowercase().contains(&needle));
                if !text_hit && !formula_hit {
                    continue;
                }
                if count >= CTL_FIND_CAP {
                    break 'outer;
                }
                if count > 0 {
                    matches.push(',');
                }
                matches.push_str("{\"sheet\":");
                matches.push_str(&si.to_string());
                matches.push_str(",\"sheet_name\":");
                json::push_str(&mut matches, &s.name);
                matches.push(',');
                // Splice in the rest of `cell_json`'s fields (dropping its
                // leading '{', which we already opened above) so `sheet`
                // and `sheet_name` lead, exactly like xlsxy's `find`.
                let cj = self.ctl_cell_json(r, c, cell);
                matches.push_str(&cj[1..]);
                count += 1;
            }
        }
        let mut out = String::from("{\"query\":");
        json::push_str(&mut out, query);
        out.push_str(",\"count\":");
        out.push_str(&count.to_string());
        out.push_str(",\"matches\":[");
        out.push_str(&matches);
        out.push_str("]}");
        Ok(out)
    }

    /// `{}` -> `{sheets, active, modified}` (the host composes this with its
    /// own URI info for its `wb.path`-equivalent reply).
    fn ctl_wb_info(&self) -> String {
        let mut out = String::from("{\"sheets\":");
        out.push_str(&self.pkg.workbook.sheets.len().to_string());
        out.push_str(",\"active\":");
        out.push_str(&self.active.to_string());
        out.push_str(",\"modified\":");
        out.push_str(if self.dirty { "true" } else { "false" });
        out.push('}');
        out
    }

    /// One cell as JSON: `ref`, coordinates, the typed `value`, the formula
    /// source (with `=`), and `text` — rendered through the same style-aware
    /// `format_with` path `view_json` uses, so an agent sees the same display
    /// text the webview does (dates, currency, etc. formatted, not just the
    /// raw general-format number).
    fn ctl_cell_json(&self, row: u32, col: u32, cell: &Cell) -> String {
        let mut out = String::from("{\"ref\":");
        json::push_str(&mut out, &cell_name(row, col));
        out.push_str(",\"row\":");
        out.push_str(&row.to_string());
        out.push_str(",\"col\":");
        out.push_str(&col.to_string());
        out.push_str(",\"value\":");
        ctl_push_value(&mut out, &cell.value);
        out.push_str(",\"text\":");
        json::push_str(&mut out, &self.ctl_cell_text(cell));
        if let Some(f) = &cell.formula {
            out.push_str(",\"formula\":");
            json::push_str(&mut out, &format!("={f}"));
        }
        out.push('}');
        out
    }

    /// The style-aware display text of a cell, via the same `format_with`
    /// path `view_json` renders the grid through.
    fn ctl_cell_text(&self, cell: &Cell) -> String {
        let wb = &self.pkg.workbook;
        let xf = wb.styles.xf(cell.style);
        format_with(&xf, &cell.value, wb.date1904)
    }

    /// [`Session::ctl_cell_json`] plus — additively, present only when set —
    /// a `format` object (see [`ctl_format_json`]). Used by `cell.get` ONLY:
    /// the read-back exists for read-modify-write, not for bulk reads
    /// (`sheet.read`/`find`) or the busiest mutating verb (`cell.set`), which
    /// all still go through the plain [`Session::ctl_cell_json`] above.
    /// Mirrors xlsxy control.rs's `cell_json_with_format`.
    fn ctl_cell_json_with_format(&self, row: u32, col: u32, cell: &Cell) -> String {
        let mut out = self.ctl_cell_json(row, col, cell);
        if let Some(fmt) = ctl_format_json(&self.pkg.workbook.styles, cell.style) {
            // `out` always ends in the cell object's closing '}' — splice
            // the format object in just before it.
            out.pop();
            out.push_str(",\"format\":");
            out.push_str(&fmt);
            out.push('}');
        }
        out
    }

    /// Resolve the `sheet` arg (index or name) to a sheet index; default =
    /// active.
    fn ctl_sheet_arg(&self, args: &json::Json) -> Result<usize, String> {
        let wb = &self.pkg.workbook;
        match args.get("sheet") {
            None | Some(json::Json::Null) => Ok(self.active),
            Some(json::Json::Num(_)) => {
                let i = args.get_usize("sheet").ok_or("bad sheet index")?;
                if i < wb.sheets.len() {
                    Ok(i)
                } else {
                    Err(format!(
                        "sheet {i} out of bounds ({} sheets)",
                        wb.sheets.len()
                    ))
                }
            }
            Some(json::Json::Str(name)) => wb
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("no sheet named '{name}'")),
            Some(_) => Err("'sheet' must be an index or a name".into()),
        }
    }

    /// Like [`Session::ctl_sheet_arg`], but the `sheet` key must be present —
    /// for destructive/renaming ops that shouldn't silently default to
    /// "whichever sheet is active" if the caller forgot the arg.
    fn ctl_sheet_arg_required(&self, args: &json::Json) -> Result<usize, String> {
        if matches!(args.get("sheet"), None | Some(json::Json::Null)) {
            return Err("needs a 'sheet' (index or name)".into());
        }
        self.ctl_sheet_arg(args)
    }

    /// A clock-free ISO-8601 timestamp for threaded-comment authoring. Unlike
    /// xlsxy's terminal `iso_now` (reads the OS clock via `SystemTime::now`),
    /// a wasm/webview host has no OS clock to read safely, so this reuses the
    /// engine's own host-supplied `clock` (the same value `NOW()`/`TODAY()`
    /// formulas see, set via the `"clock"` dispatch command), falling back to
    /// the Excel epoch when it hasn't been set — comment timestamps aren't
    /// wire-visible (`comment.list` doesn't return `date`), so this only
    /// affects the `dT=` attribute inside the threaded-comment XML part.
    fn ctl_iso_now(&self) -> String {
        let serial = self.engine.clock.unwrap_or(1.0);
        match gridcore::sheet::serial_to_parts(serial, false) {
            Some(p) => format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                p.year, p.month, p.day, p.hour, p.minute, p.second
            ),
            None => "1899-12-31T00:00:00Z".to_string(),
        }
    }

    // -----------------------------------------------------------------
    // Task 6: wave-1 read verbs
    // -----------------------------------------------------------------

    /// `{}` -> `{comments:[{sheet,ref,author,text}]}` — every comment in the
    /// workbook, flattened in `SheetPackage::comments`'s reply order (sheet,
    /// then row, then column). Mirrors xlsxy control.rs's `comment_list`.
    fn ctl_comment_list(&self) -> String {
        let comments = self.pkg.comments();
        let mut out = String::from("{\"comments\":[");
        for (i, c) in comments.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"sheet\":");
            out.push_str(&c.sheet.to_string());
            out.push_str(",\"ref\":");
            json::push_str(&mut out, &cell_name(c.row, c.col));
            out.push_str(",\"author\":");
            json::push_str(&mut out, &c.author);
            out.push_str(",\"text\":");
            json::push_str(&mut out, &c.text);
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// `{sheet?}` -> `{sheet, csv}` (display-formatted, RFC-4180). Mirrors
    /// xlsxy control.rs's `wb_export_csv`.
    fn ctl_wb_export_csv(&self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let wb = &self.pkg.workbook;
        let csv = sheet_to_csv(&wb.sheets[si], &wb.styles, wb.date1904);
        let mut out = String::from("{\"sheet\":");
        out.push_str(&si.to_string());
        out.push_str(",\"csv\":");
        json::push_str(&mut out, &csv);
        out.push('}');
        Ok(out)
    }

    /// Ad-hoc, read-only pivot over `range`: no workbook mutation, computed
    /// straight from a [`Frame`] snapshot via `gridcore::frame::pivot`.
    /// Mirrors xlsxy control.rs's `sheet_pivot`.
    fn ctl_sheet_pivot(&self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let rg = args.get_str("range").ok_or("sheet.pivot needs a 'range'")?;
        let (r1, c1, r2, c2) = ctl_parse_range(rg)?;
        let frame = Frame::from_range(&self.pkg.workbook, si, (r1, c1, r2, c2));

        // The MCP schema marks `rows` required; the code tolerates its absence
        // (defaults to empty). The schema is the contract — this leniency is
        // deliberate slack, not a documented behavior to rely on.
        let rows = ctl_names_arg(args, "rows");
        let cols = ctl_names_arg(args, "cols");
        let values_json = args
            .get("values")
            .and_then(json::Json::as_array)
            .ok_or("sheet.pivot needs a 'values' array")?;
        let values = values_json
            .iter()
            .map(|v| ctl_parse_measure_arg("sheet.pivot", v))
            .collect::<Result<Vec<_>, _>>()?;

        let spec = pivot_spec_from_names(&frame, &rows, &cols, &values)
            .map_err(|col| format!("sheet.pivot: unknown column '{col}'"))?;
        let out = pivot(&frame, &spec);
        let table = pivot_table_strings(&out);
        let mut s = String::from("{\"table\":[");
        for (i, row) in table.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('[');
            for (j, cell) in row.iter().enumerate() {
                if j > 0 {
                    s.push(',');
                }
                json::push_str(&mut s, cell);
            }
            s.push(']');
        }
        s.push_str("]}");
        Ok(s)
    }

    /// Create a REAL, persistent workbook pivot — arg shape identical to
    /// the ad-hoc `sheet.pivot` (same header-name resolution, same 11 agg
    /// strings, same unknown-column error family), plus an optional
    /// destination `name`. Mirrors xlsxy control.rs's `pivot_create`
    /// byte-for-byte (same error strings, same reply keys `{sheet, name}`),
    /// plus the wasm-only `inverse`/`undoSteps` envelope (see the
    /// bucket-table doc comment above). Builds it via
    /// [`gridcore::xlsx::SheetPackage::create_pivot`] and lands the output
    /// on a NEW sheet.
    ///
    /// Undo: clears history like `sheet.import-csv` — see the bucket-table
    /// doc comment's `pivot.create` entry. The declared inverse is
    /// `sheet.remove` on the returned `sheet` index; `SheetPackage::
    /// remove_sheet` already cascades pivot removal (Task 3), so no
    /// separate pivot-removal verb is needed.
    fn ctl_pivot_create(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let rg = args
            .get_str("range")
            .ok_or("pivot.create needs a 'range'")?;
        let (r1, c1, r2, c2) = ctl_parse_range(rg)?;
        let frame = Frame::from_range(&self.pkg.workbook, si, (r1, c1, r2, c2));
        if frame.names.is_empty() || frame.rows() == 0 {
            return Err("pivot.create: the range needs a header row and data rows".into());
        }

        // Same leniency note as sheet.pivot: the MCP schema marks `rows`
        // required; the code tolerates its absence (defaults to empty).
        let rows = ctl_names_arg(args, "rows");
        let cols = ctl_names_arg(args, "cols");
        let values_json = args
            .get("values")
            .and_then(json::Json::as_array)
            .ok_or("pivot.create needs a 'values' array")?;
        let values = values_json
            .iter()
            .map(|v| ctl_parse_measure_arg("pivot.create", v))
            .collect::<Result<Vec<_>, _>>()?;
        if values.is_empty() {
            return Err("pivot.create needs at least one value field".into());
        }
        let spec = pivot_spec_from_names(&frame, &rows, &cols, &values)
            .map_err(|col| format!("pivot.create: unknown column '{col}'"))?;

        let sheet_name = match args.get_str("name") {
            Some(n) => {
                if n.is_empty() || n.contains(['[', ']', '*', '?', ':', '/', '\\']) {
                    return Err("invalid sheet name".into());
                }
                if self.pkg.workbook.sheet_index(n).is_some() {
                    return Err(format!("pivot.create: sheet name '{n}' is already taken"));
                }
                n.to_string()
            }
            None => ctl_unique_pivot_name(&self.pkg.workbook),
        };

        let source = gridcore::pivot::PivotSource::Range {
            sheet: self.pkg.workbook.sheets[si].name.clone(),
            rect: (r1, c1, r2, c2),
        };
        let idx = self
            .pkg
            .create_pivot(source, &frame, &spec, &sheet_name)
            .ok_or("pivot.create: could not create the pivot")?;
        let dest = self.pkg.workbook.pivots[idx].sheet;

        // Same "can't be a cell-level undo" reasoning as sheet.import-csv.
        self.undo.clear();
        self.redo.clear();
        self.rebuild_engine();
        self.dirty = true;
        let mut out = String::from("{\"sheet\":");
        out.push_str(&dest.to_string());
        out.push_str(",\"name\":");
        json::push_str(&mut out, &sheet_name);
        out.push_str(",\"inverse\":{\"verb\":\"sheet.remove\",\"args\":{\"sheet\":");
        out.push_str(&dest.to_string());
        out.push_str("}},\"undoSteps\":0}");
        Ok(out)
    }

    /// Side-effect-free formula preview: evaluates `formula` against the live
    /// workbook at `ref` (default A1) without writing anywhere. Mirrors
    /// xlsxy control.rs's `formula_eval`.
    fn ctl_formula_eval(&self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let formula = args
            .get_str("formula")
            .ok_or("formula.eval needs a 'formula'")?;
        let body = formula.strip_prefix('=').unwrap_or(formula);
        let (r, c) = match args.get_str("ref") {
            Some(rf) => parse_cell_name(rf.trim()).ok_or_else(|| format!("bad cell ref '{rf}'"))?,
            None => (0, 0),
        };
        let v = eval_formula_at(&self.pkg.workbook, si, r, c, body);
        let mut out = String::from("{\"value\":");
        ctl_push_formula_value(&mut out, &v);
        out.push_str(",\"text\":");
        json::push_str(&mut out, &ctl_formula_value_text(&v));
        out.push('}');
        Ok(out)
    }

    /// `{range, sheet?}` -> `{sum,count,countNums,average,min,max}`. Mirrors
    /// xlsxy control.rs's `sheet_stats`.
    fn ctl_sheet_stats(&self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let rg = args.get_str("range").ok_or("sheet.stats needs a 'range'")?;
        let (r1, c1, r2, c2) = ctl_parse_range(rg)?;
        let s = &self.pkg.workbook.sheets[si];
        let mut vals = Vec::new();
        for r in r1..=r2 {
            for c in c1..=c2 {
                vals.push(
                    s.cell(r, c)
                        .map(|cl| cell_to_value(&cl.value))
                        .unwrap_or(Value::Empty),
                );
            }
        }
        let st = range_stats(&vals);
        let mut out = String::from("{\"sum\":");
        json::push_num(&mut out, st.sum);
        out.push_str(",\"count\":");
        out.push_str(&st.count.to_string());
        out.push_str(",\"countNums\":");
        out.push_str(&st.count_nums.to_string());
        out.push_str(",\"average\":");
        json::push_num(&mut out, st.average);
        out.push_str(",\"min\":");
        json::push_num(&mut out, st.min);
        out.push_str(",\"max\":");
        json::push_num(&mut out, st.max);
        out.push('}');
        Ok(out)
    }

    /// `{}` -> `{charts:[{kind,title?,categories,series:[{name?,values}]}]}`
    /// — every chart, read straight from each sheet's already-parsed
    /// `drawings` (populated at load time). Mirrors xlsxy control.rs's
    /// `chart_list`; no re-parsing.
    fn ctl_chart_list(&self) -> String {
        let mut charts = String::new();
        let mut count = 0usize;
        for s in &self.pkg.workbook.sheets {
            for d in &s.drawings {
                let DrawingKind::Chart(cd) = &d.kind else {
                    continue;
                };
                if count > 0 {
                    charts.push(',');
                }
                count += 1;
                charts.push_str("{\"kind\":");
                json::push_str(&mut charts, &cd.kind);
                if !cd.title.is_empty() {
                    charts.push_str(",\"title\":");
                    json::push_str(&mut charts, &cd.title);
                }
                charts.push_str(",\"categories\":[");
                for (i, cat) in cd.categories.iter().enumerate() {
                    if i > 0 {
                        charts.push(',');
                    }
                    json::push_str(&mut charts, cat);
                }
                charts.push_str("],\"series\":[");
                for (i, ser) in cd.series.iter().enumerate() {
                    if i > 0 {
                        charts.push(',');
                    }
                    charts.push('{');
                    if !ser.name.is_empty() {
                        charts.push_str("\"name\":");
                        json::push_str(&mut charts, &ser.name);
                        charts.push(',');
                    }
                    charts.push_str("\"values\":[");
                    for (j, v) in ser.values.iter().enumerate() {
                        if j > 0 {
                            charts.push(',');
                        }
                        json::push_num(&mut charts, *v);
                    }
                    charts.push_str("]}");
                }
                charts.push_str("]}");
            }
        }
        format!("{{\"charts\":[{charts}]}}")
    }

    /// `{}` -> `{pivots:[{sheet,rows,cols,values}]}` — every persistent pivot
    /// table, summarized (row/column field names, value display names).
    /// Mirrors xlsxy control.rs's `pivot_list`.
    fn ctl_pivot_list(&self) -> String {
        let mut out = String::from("{\"pivots\":[");
        for (i, p) in self.pkg.workbook.pivots.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let field_name = |idx: &usize| p.fields.get(*idx).cloned().unwrap_or_default();
            out.push_str("{\"sheet\":");
            out.push_str(&p.sheet.to_string());
            out.push_str(",\"rows\":[");
            for (j, fi) in p.row_fields.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                json::push_str(&mut out, &field_name(fi));
            }
            out.push_str("],\"cols\":[");
            for (j, fi) in p.col_fields.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                json::push_str(&mut out, &field_name(fi));
            }
            out.push_str("],\"values\":[");
            for (j, df) in p.data_fields.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                json::push_str(&mut out, &df.name);
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }

    // -----------------------------------------------------------------
    // Task 6: wave-1 mutating verbs
    //
    // Undo-mechanism buckets (Task 7's contract — see the module doc note
    // and the report for the full table):
    //   - `range.set`, `sheet.rename`, `row.*`, `col.*`, `wb.replace-all`:
    //     ONE true wasm-undo-stack entry -> reply carries `"undoSteps":1`
    //     (or `0` for range.set's empty-batch no-op). A single
    //     `dispatch("undo")` genuinely reverts the whole edit.
    //   - `comment.add`/`comment.remove`: NEVER on the undo stack (comment
    //     data lives in package parts outside `Cell`/`Sheet`/`WbSnapshot`) ->
    //     reply carries an internal `"inverse":{"verb":…,"args":{…}}`
    //     describing the host-orchestrated inverse. Both directions invert to
    //     the SAME internal `comment.replace-thread` verb carrying the cell's
    //     captured PRE-op thread (every message, in order): `comment.add`
    //     appends a reply, so its inverse must restore the exact prior thread
    //     (not wipe the whole ref); `comment.remove` drops the whole ref, so
    //     its inverse must re-add every message (not just the first). This is
    //     faithful in both directions — an earlier version re-added only the
    //     first message on remove and wiped the pre-existing thread on add,
    //     silently losing user data on undo.
    //   - `sheet.add`: reuses the SAME true wasm-undo-stack machinery the
    //     interactive `"sheet\tadd"` dispatch command already has
    //     (`UndoAction::SheetAdd`, whose inverse is the real
    //     `SheetPackage::remove_sheet` — proven by
    //     `sheet_add_undo_survives_save` above) -> `"undoSteps":1`. Unlike
    //     xlsxy's terminal (which has no such stack entry and clears
    //     history for `sheet.add`), gridwasm already has a true inverse, so
    //     it's used directly rather than falling back to a weaker bucket.
    //   - `sheet.remove`, `sheet.import-csv`: NEVER on the undo stack AND
    //     ACTIVELY CLEAR existing history (same reasoning as `sheet.add`'s
    //     package-parts churn, but there is no cheap true inverse for
    //     *removing* or *importing* arbitrary content) -> reply carries
    //     `"undoSteps":0` plus an `"inverse"`. `sheet.import-csv`'s inverse
    //     (`sheet.remove` of the very sheet it just created) is EXACT — the
    //     sheet never existed before, so deleting it is a full reversal.
    //     `sheet.remove`'s inverse is `sheet.restore-removed` (an
    //     INTERNAL-ONLY verb — Task 7 must not list it in `wasmVerbs`, so an
    //     external agent calling it through `CtlServer` still gets "unknown
    //     verb"), backed by the bridge-level `Session::removed_sheet_stash`
    //     ([`RemovedSheetStash`]) captured just before removal: cells,
    //     formulas, drawings, and comments all round-trip losslessly. The
    //     one documented gap is the sheet's numeric INDEX — see
    //     `ctl_sheet_restore_removed`'s doc comment for why a bridge-only
    //     change can't splice it back into its original position. (An
    //     earlier version of this bucket used a lossy `sheet.add`-by-name
    //     inverse instead — replaced after review flagged that it would
    //     silently resurrect an EMPTY sheet as "undo".)
    //   - `cell.format`: Task 3's empirical bucket A carries over — it goes
    //     through [`Session::apply_styles`] (only each cell's `style` index
    //     changes; value/formula/spill untouched), which pushes ONE `Cells`
    //     group (`styles_only`) onto the same true wasm undo stack
    //     `range.set` uses -> `"undoSteps":1`, unconditionally (a range always
    //     covers >=1 cell, unlike `range.set`'s possibly-empty `rows` batch).
    //   - `col.width`: Task 3 found the TUI's own F7/F8 width-adjust keys
    //     never push onto xlsxy's undo stack at all (no true inverse exists
    //     to reuse) -> here too, NOT pushed onto gridwasm's `undo`/`redo`
    //     stack. Unlike `comment.add`/`comment.remove` (whose inverse only
    //     needs to restore an opaque snapshot), a column width has a
    //     trivially cheap TRUE inverse — the prior width, captured here
    //     before mutating — so this follows the Wave-1 three-bucket
    //     playbook (bucket B): reply carries `"undoSteps":0` plus an
    //     `"inverse"` that is itself another `col.width` call (carrying the
    //     prior width), whose own reply chains a working redo. `col` echoes
    //     NUMERICALLY (0-based) per Task 3's locked reply-shape decision —
    //     there is no paired ref-style field here to carry a letter form.
    //   - `pivot.create` (Wave-3 Task 4): same bucket as `sheet.import-csv` —
    //     a brand-new sheet plus a pivot-part registration isn't a
    //     cell-level edit the wasm undo stack can invert, and there's no
    //     cheap true inverse for creating a full pivot layout, so history is
    //     cleared -> `"undoSteps":0` plus an `"inverse"` of `sheet.remove` on
    //     the created sheet. That inverse is EXACT and "both or neither" for
    //     free: the sheet never existed before (so removing it is a full
    //     reversal) AND `SheetPackage::remove_sheet` already cascades pivot
    //     removal (Task 3), so no separate pivot-removal verb is needed —
    //     `pivot.create`'s own empirically-determined bucket (Task 3's
    //     report §3), reused here rather than inventing a new one. Restoring
    //     that same sheet afterward (`sheet.restore-removed`) must bring the
    //     pivot back too, not just the sheet — see `RemovedSheetStash`'s
    //     `pivots` field and `ctl_reregister_pivot`.
    // -----------------------------------------------------------------

    /// `{ref,text,author?,sheet?}` -> `{sheet,ref,undoSteps:0}` — plus the
    /// internal `inverse`, a `comment.replace-thread` that restores the cell's
    /// PRE-ADD thread (captured here before the append). `comment.add` appends
    /// a reply when a thread already exists, so a blind `comment.remove`
    /// inverse would wipe the user's pre-existing thread on undo; replaying the
    /// captured thread (empty when the cell was blank) reverses ONLY this add.
    /// Not pushed onto the undo stack: comment data lives in package parts
    /// (`xl/threadedComments/…`, `xl/persons/…`) outside `Cell`/`Sheet`.
    /// `undoSteps` is unconditionally present (see `ctl_comment_remove`'s
    /// doc comment for why).
    fn ctl_comment_add(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let (r, c) = ctl_ref_arg(args)?;
        let text = args.get_str("text").ok_or("comment.add needs 'text'")?;
        if text.is_empty() {
            return Err("comment.add needs non-empty 'text'".into());
        }
        let author = args.get_str("author").unwrap_or(CTL_DEFAULT_COMMENT_AUTHOR);
        // Snapshot the cell's PRE-ADD thread so the inverse restores exactly
        // it (empty = pure removal), rather than dropping the whole ref.
        let pre = self.ctl_comment_messages_at(si, r, c);
        let when = self.ctl_iso_now();
        if !self.pkg.add_threaded_comment(si, r, c, author, text, &when) {
            return Err(
                "comment.add: this sheet's XML is damaged where the comment would go".into(),
            );
        }
        self.dirty = true;
        let cref = cell_name(r, c);
        let mut out = String::from("{\"sheet\":");
        out.push_str(&si.to_string());
        out.push_str(",\"ref\":");
        json::push_str(&mut out, &cref);
        out.push_str(",\"inverse\":");
        out.push_str(&ctl_replace_thread_inverse(si, &cref, &pre));
        out.push_str(",\"undoSteps\":0}");
        Ok(out)
    }

    /// `{ref,sheet?}` -> `{removed:bool,undoSteps:0}` — plus, only when
    /// something was actually removed, the internal `inverse`, a
    /// `comment.replace-thread` carrying the WHOLE pre-remove thread (every
    /// message, in order) so undo restores the entire conversation, not just
    /// its first message. `comment.remove` drops the whole ref, so a
    /// single-message `comment.add` inverse would silently lose the rest of a
    /// multi-message thread. Not on the undo stack, same reasoning as
    /// `comment.add`. `undoSteps` is unconditionally present (always `0`
    /// here) so every mutating verb's reply carries it uniformly — Task 7
    /// can check it first regardless of bucket, falling back to `inverse`
    /// only when it's `0` and an `inverse` is actually present.
    fn ctl_comment_remove(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let (r, c) = ctl_ref_arg(args)?;
        // Snapshot the WHOLE pre-remove thread so the inverse restores every
        // message; empty = nothing here, so no change and no inverse.
        let pre = self.ctl_comment_messages_at(si, r, c);
        let existed = !pre.is_empty();
        if existed {
            self.pkg.remove_comment(si, r, c);
            self.dirty = true;
        }
        let mut out = String::from("{\"removed\":");
        out.push_str(if existed { "true" } else { "false" });
        if existed {
            let cref = cell_name(r, c);
            out.push_str(",\"inverse\":");
            out.push_str(&ctl_replace_thread_inverse(si, &cref, &pre));
        }
        out.push_str(",\"undoSteps\":0}");
        Ok(out)
    }

    /// Every `(author, text)` at `(si, r, c)`, in `comments()` order (threaded
    /// replies keep their insertion order). The whole-thread snapshot behind a
    /// faithful `comment.replace-thread` inverse for comment.add/remove.
    fn ctl_comment_messages_at(&self, si: usize, r: u32, c: u32) -> Vec<(String, String)> {
        self.pkg
            .comments()
            .into_iter()
            .filter(|cm| cm.sheet == si && cm.row == r && cm.col == c)
            .map(|cm| (cm.author, cm.text))
            .collect()
    }

    /// `{sheet?,ref,messages:[{author,text},...]}` -> `{sheet,ref,undoSteps:0}`
    /// — plus the internal `inverse`, another `comment.replace-thread`
    /// carrying this call's PRE-REPLACE thread (so undo/redo chains work).
    /// **INTERNAL-ONLY**, like [`ctl_sheet_restore_removed`](Self::ctl_sheet_restore_removed):
    /// it is deliberately absent from `wasmVerbs`, the MCP tool maps, and the
    /// docs, so an external agent calling it through `CtlServer` still gets
    /// "unknown verb 'comment.replace-thread'"; it is reached only as the
    /// host-orchestrated `inverse` of comment.add/remove (and of itself).
    ///
    /// Removes the whole ref, then re-adds `messages` in order (empty list =
    /// pure removal). STATELESS — the args carry the thread, no session stash,
    /// so redo chains never desync. Re-adds through `add_threaded_comment`
    /// (comment.add's own primitive): a restored thread is threaded regardless
    /// of the removed thread's original storage form. Message author/text are
    /// preserved exactly (what `comment.list` reports and the tests compare);
    /// the threaded-vs-legacy distinction is intentionally outside this
    /// inverse's fidelity contract, matching the `{author,text}` message shape.
    fn ctl_comment_replace_thread(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let (r, c) = ctl_ref_arg(args)?;
        let messages_json = args
            .get("messages")
            .and_then(json::Json::as_array)
            .ok_or("comment.replace-thread needs a 'messages' array")?;
        // Parse every message BEFORE mutating, so a malformed entry leaves the
        // existing thread untouched.
        let mut messages: Vec<(String, String)> = Vec::new();
        for m in messages_json {
            let author = m
                .get_str("author")
                .unwrap_or(CTL_DEFAULT_COMMENT_AUTHOR)
                .to_string();
            let text = m
                .get_str("text")
                .ok_or("comment.replace-thread: each message needs a 'text'")?
                .to_string();
            messages.push((author, text));
        }
        // Snapshot the PRE-REPLACE thread for this call's own inverse (redo).
        let pre = self.ctl_comment_messages_at(si, r, c);
        self.pkg.remove_comment(si, r, c);
        for (author, text) in &messages {
            let when = self.ctl_iso_now();
            self.pkg.add_threaded_comment(si, r, c, author, text, &when);
        }
        self.dirty = true;
        let cref = cell_name(r, c);
        let mut out = String::from("{\"sheet\":");
        out.push_str(&si.to_string());
        out.push_str(",\"ref\":");
        json::push_str(&mut out, &cref);
        out.push_str(",\"inverse\":");
        out.push_str(&ctl_replace_thread_inverse(si, &cref, &pre));
        out.push_str(",\"undoSteps\":0}");
        Ok(out)
    }

    /// `{start,rows:[[string]],sheet?}` -> `{set,undoSteps}`, each string
    /// typed the way `set` types it (gridcore::entry) — ATOMIC: every formula
    /// and every length in the batch is checked *before* anything is applied,
    /// so a bad formula or an over-long entry anywhere leaves the sheet (and
    /// the undo stack) completely untouched. The whole block lands as one [`Session::apply`] call, i.e.
    /// one true wasm-undo-stack group (`undoSteps:1`; `0` only for a
    /// genuinely empty `rows` batch, which `apply` no-ops on). Mirrors xlsxy
    /// control.rs's `range_set`.
    fn ctl_range_set(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let start = args.get_str("start").ok_or("range.set needs a 'start'")?;
        let (r0, c0) =
            parse_cell_name(start.trim()).ok_or_else(|| format!("bad cell ref '{start}'"))?;
        let rows_json = args
            .get("rows")
            .and_then(json::Json::as_array)
            .ok_or("range.set needs a 'rows' array")?;

        let mut entries = Vec::new();
        for (i, row) in rows_json.iter().enumerate() {
            let row_arr = row
                .as_array()
                .ok_or("range.set: each row must be an array of strings")?;
            for (j, cellv) in row_arr.iter().enumerate() {
                let text = cellv
                    .as_str()
                    .ok_or("range.set: each cell must be a string")?;
                entries.push((r0 + i as u32, c0 + j as u32, text.to_string()));
            }
        }

        // Pass 1: validate every formula and length before touching anything.
        for (r, c, text) in &entries {
            let wb = &self.pkg.workbook;
            if let Some(body) = gridcore::entry::typed_formula(wb, si, *r, *c, text) {
                Engine::validate(body).map_err(|e| {
                    format!("range.set: formula error at {}: {e}", cell_name(*r, *c))
                })?;
            }
            gridcore::entry::check_len(text)
                .map_err(|e| format!("range.set: {e} at {}", cell_name(*r, *c)))?;
        }

        // Pass 2: every entry validated — build the changes and apply as one
        // undo group, on the target sheet (temporarily swapping `active`,
        // same trick `ctl_cell_set` already uses, since `apply` targets
        // `self.active`).
        let today = self.engine.clock;
        let mut changes: Vec<(u32, u32, Cell)> = Vec::with_capacity(entries.len());
        for (r, c, text) in entries {
            let cell = gridcore::entry::entry_cell(&mut self.pkg.workbook, si, r, c, &text, today)
                .map_err(|e| format!("range.set: {e} at {}", cell_name(r, c)))?;
            changes.push((r, c, cell));
        }
        let prev_active = self.active;
        self.active = si;
        let n = changes.len();
        let undo_steps = if changes.is_empty() { 0 } else { 1 };
        let applied = self.apply(changes);
        self.active = prev_active;
        if !applied {
            self.err = None;
            return Err(format!("range.set: {PART_OF_ARRAY}"));
        }
        let mut out = String::from("{\"set\":");
        out.push_str(&n.to_string());
        out.push_str(",\"undoSteps\":");
        out.push_str(&undo_steps.to_string());
        out.push('}');
        Ok(out)
    }

    /// `{range,patch,sheet?}` -> `{formatted,undoSteps:1}` — sets `patch`
    /// over every cell in `range` via the shared `gridcore::format` helpers
    /// (`apply_patch_to_xf`/`FormatPatch::parse`), landing as ONE
    /// [`Session::apply_styles`] call — the SAME true wasm-undo-stack `Cells`
    /// group kind `range.set` uses (Task 3's empirical bucket A). Only each
    /// cell's style index changes: value, formula and spill are never
    /// re-entered, so a spilled block stays spilled. `undoSteps` is
    /// unconditionally `1`: unlike `range.set`'s possibly-empty `rows`
    /// batch, a parsed range always covers >=1 cell. Mirrors xlsxy
    /// control.rs's `cell_format` field-for-field (snapshot-then-mutate,
    /// same `Styles::intern` reuse/dedup).
    fn ctl_cell_format(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let rg = args.get_str("range").ok_or("cell.format needs a 'range'")?;
        let (r1, c1, r2, c2) = ctl_parse_range(rg)?;
        // Reject an oversized range BEFORE materializing a Cell per coordinate
        // or touching the undo stack — see
        // `gridcore::format::check_format_range_cap`; mirrors xlsxy's
        // `cell_format` byte-for-byte, including the error string.
        gridcore::format::check_format_range_cap(r1, c1, r2, c2)?;
        let patch_arg = args.get("patch").ok_or("cell.format needs a 'patch'")?;
        let pairs = ctl_patch_pairs(patch_arg)?;
        let patch = FormatPatch::parse(&pairs)?;

        let snapshot: Vec<(u32, u32, u32)> = {
            let sheet = &self.pkg.workbook.sheets[si];
            let mut v = Vec::new();
            for r in r1..=r2 {
                for c in c1..=c2 {
                    v.push((r, c, sheet.cell(r, c).map_or(0, |cl| cl.style)));
                }
            }
            v
        };
        let mut styles = Vec::with_capacity(snapshot.len());
        for (r, c, cur) in snapshot {
            let base_xf = self.pkg.workbook.styles.xf(cur);
            let new_xf = apply_patch_to_xf(&base_xf, &patch);
            let idx = self.pkg.workbook.styles.intern(new_xf);
            styles.push((r, c, idx));
        }
        let formatted = styles.len();
        self.apply_styles(si, styles);
        Ok(format!("{{\"formatted\":{formatted},\"undoSteps\":1}}"))
    }

    /// `{col,width,sheet?}` -> `{col,width,undoSteps:0}` — plus the internal
    /// `inverse`, a `col.width` call carrying the PRIOR width (captured
    /// before mutating) — Wave-1's three-bucket playbook, bucket B: Task 3
    /// found the TUI's own F7/F8 width-adjust keys never push onto xlsxy's
    /// undo stack (no true inverse to reuse), and this mirrors that exactly
    /// — NOT pushed onto gridwasm's `undo`/`redo` stack either. Unlike
    /// `comment.add`/`comment.remove` (an opaque-snapshot inverse), a column
    /// width has a trivially cheap TRUE inverse, so the inverse's own reply
    /// chains a working redo (apply it again to get back to the new width).
    /// `col` echoes NUMERICALLY (0-based), matching Task 3's locked
    /// reply-shape decision — there is no paired ref-style field here to
    /// carry a letter form. Mirrors xlsxy control.rs's `col_width`.
    fn ctl_col_width(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg(args)?;
        let col = ctl_col_arg(args)?;
        let width = args
            .get("width")
            .and_then(json::Json::as_f64)
            .ok_or("col.width needs a 'width' number")?;
        if !(width.is_finite() && width > 0.0) {
            return Err("col.width: 'width' must be positive".to_string());
        }
        let prior = self.pkg.workbook.sheets[si].col_width(col);
        self.pkg.workbook.sheets[si].set_col_width(col, width);
        self.dirty = true;
        let mut out = String::from("{\"col\":");
        out.push_str(&col.to_string());
        out.push_str(",\"width\":");
        json::push_num(&mut out, width);
        out.push_str(",\"inverse\":");
        out.push_str(&ctl_col_width_inverse(si, col, prior));
        out.push_str(",\"undoSteps\":0}");
        Ok(out)
    }

    /// `{name?}` -> `{sheet,name,undoSteps:1}`. Reuses the exact
    /// `UndoAction::SheetAdd` machinery the interactive `"sheet\tadd"`
    /// dispatch command uses (see the bucket-table doc comment above) — but
    /// invoked directly rather than through the tab-delimited dispatch
    /// string, so it neither disturbs `active`/`cur`/`anchor` (an
    /// agent-driven background edit shouldn't yank a human's live view, same
    /// principle xlsxy's Task 5 established) nor silently no-ops on an
    /// empty-string `name` (the dispatch command's `!name.is_empty()` guard
    /// would otherwise turn `{"name":""}` into a no-op that still reports
    /// success).
    fn ctl_sheet_add(&mut self, args: &json::Json) -> Result<String, String> {
        let requested = args.get_str("name").unwrap_or("Sheet");
        let name = ctl_unique_sheet_name(&self.pkg.workbook, requested);
        let idx = self.pkg.add_sheet(&name);
        self.rebuild_engine();
        self.undo.push(UndoAction::SheetAdd {
            idx,
            name: name.clone(),
        });
        self.redo.clear();
        self.dirty = true;
        let mut out = String::from("{\"sheet\":");
        out.push_str(&idx.to_string());
        out.push_str(",\"name\":");
        json::push_str(&mut out, &name);
        out.push_str(",\"undoSteps\":1}");
        Ok(out)
    }

    /// `{sheet}` -> `{removed:true,undoSteps:0}` — plus the internal
    /// `inverse`, `{"verb":"sheet.restore-removed","args":{"name":"<removed
    /// sheet name>"}}` (see the bucket-table doc comment above and
    /// [`ctl_sheet_restore_removed`](Self::ctl_sheet_restore_removed) for
    /// what that verb does). The `name` gives the inverse an IDENTITY: the
    /// stash is single-slot, so an intervening `sheet.remove` (e.g. a user
    /// undo of an agent import) overwrites it; a named restore can then detect
    /// the mismatch and refuse rather than silently resurrecting the wrong
    /// sheet. Before removing, snapshots the sheet's full
    /// `Sheet` value, its comments, its sheet-scoped defined names, AND
    /// every pivot whose output lived on it (Task 4 fix — `remove_sheet`'s
    /// pivot cascade, added for `pivot.create`'s inverse, would otherwise
    /// silently outrun this stash), into `Session::removed_sheet_stash`
    /// ([`RemovedSheetStash`]), so the inverse is a genuine restore, not a
    /// name-only placeholder. Errors on
    /// the last sheet — checked FIRST, before any cloning, so a doomed call
    /// doesn't pay for a snapshot it's about to throw away. `sheet` is
    /// required (no default to active) — a destructive op shouldn't
    /// silently default to "whichever one is active". Only resets
    /// `cur`/`anchor`/the viewport when the ACTIVE sheet itself is the one
    /// removed (Task 5's xlsxy fix, mirrored here).
    fn ctl_sheet_remove(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg_required(args)?;
        // A workbook must keep at least one sheet — `SheetPackage::remove_sheet`
        // enforces this too (and is still checked below as a safety net),
        // but checking it here FIRST avoids cloning a whole `Sheet` (plus
        // walking comments/defined names) on a call that's guaranteed to
        // fail anyway.
        if self.pkg.workbook.sheets.len() <= 1 {
            return Err("cannot remove the last sheet".into());
        }
        let name = self.pkg.workbook.sheets[si].name.clone();
        let sheet_snapshot = self.pkg.workbook.sheets[si].clone();
        // Comment data lives in package parts outside `Sheet` — must be
        // captured now, while `sheet_parts[si]` (and thus this sheet's own
        // `_rels`/threadedComments/persons parts) are still resolvable.
        let comments: Vec<gridcore::comments::Comment> = self
            .pkg
            .comments()
            .into_iter()
            .filter(|cm| cm.sheet == si)
            .collect();
        // Defined names scoped to this sheet live on `Workbook::defined_names`,
        // not `Sheet` — `remove_sheet` drops them via a `retain(|d| d.scope
        // != Some(idx))`, so they'd otherwise vanish silently too.
        let defined_names: Vec<DefinedName> = self
            .pkg
            .workbook
            .defined_names
            .iter()
            .filter(|d| d.scope == Some(si))
            .cloned()
            .collect();
        let removed_active = self.active == si;
        // Every pivot the impending `remove_sheet` cascade is about to drop
        // — same filter (`.sheet == si`) the cascade itself uses, captured
        // BEFORE the call so the stash has something to reconstitute from.
        let pivots: Vec<gridcore::pivot::Pivot> = self
            .pkg
            .workbook
            .pivots
            .iter()
            .filter(|p| p.sheet == si)
            .cloned()
            .collect();
        if !self.pkg.remove_sheet(si) {
            return Err("cannot remove the last sheet".into());
        }
        if self.active > si {
            self.active -= 1;
        } else if removed_active {
            self.active = self.active.min(self.pkg.workbook.sheets.len() - 1);
        }
        if removed_active {
            self.cur = (0, 0);
            self.anchor = None;
            self.window.0 = 0;
            self.window.1 = 0;
        }
        // Package parts (worksheet part, content-type override, workbook
        // rel, workbook.xml entry) changed — a `WbSnapshot` (only
        // `sheets`+`defined_names`) can't represent the inverse, same
        // reasoning as `SheetAdd`'s own doc comment. Clear rather than push
        // a false entry the engine couldn't actually revert.
        self.undo.clear();
        self.redo.clear();
        self.rebuild_engine();
        self.dirty = true;
        // Single-slot: a second `sheet.remove` overwrites whatever was
        // stashed from an earlier one — only the most recent removal is
        // restorable, by design (see `RemovedSheetStash`'s doc comment).
        self.removed_sheet_stash = Some(RemovedSheetStash {
            name: name.clone(),
            sheet: sheet_snapshot,
            comments,
            defined_names,
            removed_active,
            pivots,
        });
        let mut out = String::from(
            "{\"removed\":true,\"inverse\":{\"verb\":\"sheet.restore-removed\",\"args\":{\"name\":",
        );
        json::push_str(&mut out, &name);
        out.push_str("}},\"undoSteps\":0}");
        Ok(out)
    }

    /// `{name?}` -> `{sheet,name,undoSteps:0}` — plus the internal `inverse`
    /// (`sheet.remove` on the just-restored sheet's NEW index, so a
    /// follow-up `sheet.remove` redoes the removal). The optional `name` is
    /// the IDENTITY guard: `sheet.remove`'s inverse always supplies the name
    /// of the sheet it removed, and this errors (`restore mismatch: …`) when
    /// that name doesn't match the single-slot stash — the failure mode where
    /// an intervening `sheet.remove` overwrote the stash, so a blind restore
    /// would resurrect the wrong sheet. It also errors (`cannot restore …`)
    /// when a sheet with the stashed name already exists (an intervening
    /// same-name `sheet.add`), rather than minting a duplicate name. Both
    /// checks run BEFORE the stash is consumed, so a rejected restore leaves
    /// it intact for a correct later attempt. **INTERNAL-ONLY**: this
    /// verb exists solely so the host can invoke `sheet.remove`'s declared
    /// `inverse` through the normal ctl channel — Task 7 must NOT list it in
    /// `wasmVerbs`, so an external agent calling it through `CtlServer`
    /// still gets `"unknown verb 'sheet.restore-removed'"`.
    ///
    /// Restores the single most-recently-removed sheet from
    /// `Session::removed_sheet_stash`: the full `Sheet` value (cells,
    /// formulas, drawings — everything `Sheet` itself carries), every
    /// comment (threaded and legacy), and every defined name that was
    /// scoped to the removed sheet — all captured by `ctl_sheet_remove`
    /// just before deletion. Defined names are re-inserted with `scope`
    /// re-pointed at the restored sheet's NEW index (their old index may no
    /// longer even exist). They also survive a `save`: gridcore writes a
    /// model name that has no `<definedName>` element (the restored ones lost
    /// theirs to `remove_sheet`). The restored sheet's autoFilter and its
    /// `_xlnm._FilterDatabase` are not recreated, though: the sheet gets a
    /// fresh part with no `<autoFilter>`, and a save never adds one or the
    /// name that backs it. The stash is single-slot — a second
    /// `sheet.remove` overwrites it, and a successful restore takes
    /// (clears) it via `Option::take` — so calling this with nothing
    /// stashed errors (`"nothing to restore"`).
    ///
    /// Also reconstitutes every pivot that lived on the removed sheet (Task
    /// 4 fix — see [`RemovedSheetStash::pivots`] and
    /// [`ctl_reregister_pivot`](Self::ctl_reregister_pivot)) and refreshes
    /// them immediately, so `pivot.list` shows the pivot again and its
    /// output is live without waiting for a follow-up `wb.recalc`.
    ///
    /// Active-sheet/viewport handling mirrors `sheet.remove`'s own rule in
    /// reverse: if the removed sheet WAS the active one, the restored sheet
    /// becomes active again (with a fresh cursor/viewport, matching the
    /// removal's own reset); otherwise `active`/`cur`/`anchor`/the viewport
    /// are left completely untouched — appending a sheet at the end never
    /// changes any other sheet's index, so no compensating shift is needed
    /// either way (verified by the removed-before-active and removed-active
    /// tests below).
    ///
    /// **Documented limitation**: the restored sheet is always appended at
    /// the END of the sheet list, not spliced back into its original
    /// numeric index, whenever other sheets existed after it.
    /// `SheetPackage::sheet_parts` — which must stay in lockstep,
    /// index-for-index, with `workbook.sheets` for `save_xlsx` to write each
    /// sheet's regenerated cell data into the correct XML part — is
    /// `pub(crate)` in gridcore, invisible outside that crate. Reordering
    /// `workbook.sheets` alone (the only piece gridwasm can reach) without
    /// also reordering `sheet_parts` would desync the two and corrupt the
    /// next save (wrong sheet's cells written into another sheet's XML
    /// part). True positional restore needs a small new gridcore primitive
    /// (e.g. an `insert_sheet_at`); genuinely out of reach from
    /// `gridwasm/src/bridge.rs` alone, which is this task's explicit scope.
    fn ctl_sheet_restore_removed(&mut self, args: &json::Json) -> Result<String, String> {
        // Peek at the stash name WITHOUT consuming it, so a mismatch (or a
        // name collision) leaves the stash intact for a correct later restore.
        let stashed_name = self
            .removed_sheet_stash
            .as_ref()
            .map(|s| s.name.clone())
            .ok_or("nothing to restore")?;
        // Identity guard: the inverse of `sheet.remove` names the sheet it
        // removed. If an intervening `sheet.remove` (e.g. a user undo of an
        // agent import) has overwritten the single-slot stash, the requested
        // name won't match — error rather than silently restoring the wrong
        // (most-recently-removed) sheet. `name` is optional for legacy/direct
        // callers, but `sheet.remove`'s inverse always carries it.
        if let Some(req) = args.get_str("name") {
            if req != stashed_name {
                return Err(format!(
                    "restore mismatch: stashed sheet is \"{stashed_name}\", not \"{req}\""
                ));
            }
        }
        // A sheet with the stashed name may have been re-created after removal
        // (an intervening same-name `sheet.add`). `add_sheet` doesn't dedupe,
        // so restoring would mint a duplicate sheet name — refuse instead.
        if self.pkg.workbook.sheet_index(&stashed_name).is_some() {
            return Err(format!(
                "cannot restore \"{stashed_name}\": a sheet with that name already exists"
            ));
        }
        let stash = self
            .removed_sheet_stash
            .take()
            .expect("stash presence verified above");
        let new_idx = self.pkg.add_sheet(&stash.name);
        self.pkg.workbook.sheets[new_idx] = stash.sheet;
        for cm in &stash.comments {
            if cm.threaded {
                let when = self.ctl_iso_now();
                self.pkg
                    .add_threaded_comment(new_idx, cm.row, cm.col, &cm.author, &cm.text, &when);
            } else {
                self.pkg
                    .set_comment(new_idx, cm.row, cm.col, &cm.author, &cm.text);
            }
        }
        // Re-point each name's scope at the NEW index — the old one may no
        // longer even exist (sheets above it shifted down on removal).
        for dn in stash.defined_names {
            self.pkg.workbook.defined_names.push(DefinedName {
                name: dn.name,
                scope: Some(new_idx),
                formula: dn.formula,
            });
        }
        // Reconstitute every pivot that lived on this sheet before removal
        // (see `RemovedSheetStash::pivots`'s doc comment) — `.sheet`
        // re-pointed at the restored sheet's NEW index. A pivot that fails
        // to re-register (shouldn't happen for one that was live a moment
        // ago; see `ctl_reregister_pivot`'s doc comment) is simply dropped
        // rather than aborting the whole sheet restore.
        let had_pivots = !stash.pivots.is_empty();
        for piv in stash.pivots {
            self.ctl_reregister_pivot(piv, new_idx);
        }
        if had_pivots {
            // Makes the restored output live again immediately (matching
            // `create_pivot`'s own "refresh right away" behavior), not just
            // eventually-consistent on the next `wb.recalc`.
            gridcore::pivot::refresh_pivots(&mut self.pkg.workbook);
        }
        if stash.removed_active {
            self.active = new_idx;
            self.cur = (0, 0);
            self.anchor = None;
            self.window.0 = 0;
            self.window.1 = 0;
        }
        self.rebuild_engine();
        self.dirty = true;
        let mut out = String::from("{\"sheet\":");
        out.push_str(&new_idx.to_string());
        out.push_str(",\"name\":");
        json::push_str(&mut out, &stash.name);
        out.push_str(",\"inverse\":{\"verb\":\"sheet.remove\",\"args\":{\"sheet\":");
        out.push_str(&new_idx.to_string());
        out.push_str("}},\"undoSteps\":0}");
        Ok(out)
    }

    /// Reinsert a pivot into `workbook.pivots`, pointed at `dest_sheet`, as
    /// part of undoing `remove_sheet`'s pivot cascade
    /// ([`ctl_sheet_restore_removed`]). `piv` is the FULL pivot as it stood
    /// just before removal (row/col/data fields, filters, calculated
    /// fields, grand-total/subtotal flags — every field
    /// [`RemovedSheetStash::pivots`] captured), so the reconstituted pivot
    /// is byte-identical to the destroyed one in every way that matters for
    /// refresh and `pivot.list` — with ONE deliberate exception: its
    /// underlying OPC part names (`part`/`cache_part`) are NOT preserved.
    /// `remove_pivot` deletes those parts outright (table/cache XML,
    /// content-type overrides, workbook rels), so there is nothing left to
    /// point the old names at; this reuses [`SheetPackage::add_pivot`]
    /// purely for its OPC-wiring side effects (fresh, currently-unused part
    /// names, content-type overrides, a new workbook `<pivotCache>`
    /// registration, a fresh destination-sheet rels entry) and then
    /// overwrites its minimal placeholder `Pivot` entry with the fully
    /// stashed one field-for-field. This is safe because the reconstituted
    /// pivot is always marked `edited = true`: `save_xlsx` always rewrites
    /// the table-definition XML from the model on the next save (same as
    /// any `pivot.create`-made pivot, whose freshly-minted part is likewise
    /// just a structural stub — see `SheetPackage::create_pivot`), so the
    /// fresh part's own starting content never needs to matter. Returns the
    /// new index into `workbook.pivots`; `None` when `dest_sheet` is out of
    /// range or `piv.fields` is empty (both delegated to `add_pivot`'s own
    /// guard — shouldn't occur for a pivot that was live a moment ago).
    fn ctl_reregister_pivot(
        &mut self,
        mut piv: gridcore::pivot::Pivot,
        dest_sheet: usize,
    ) -> Option<usize> {
        let default_measure =
            piv.data_fields
                .first()
                .cloned()
                .unwrap_or(gridcore::pivot::DataField {
                    name: String::new(),
                    field: 0,
                    agg: Agg::Sum,
                });
        let idx = self.pkg.add_pivot(
            piv.source.clone(),
            piv.fields.clone(),
            default_measure,
            dest_sheet,
            (piv.location.0, piv.location.1),
        )?;
        let fresh = &self.pkg.workbook.pivots[idx];
        piv.sheet = dest_sheet;
        piv.part = fresh.part.clone();
        piv.cache_part = fresh.cache_part.clone();
        piv.edited = true;
        self.pkg.workbook.pivots[idx] = piv;
        Some(idx)
    }

    /// `{sheet,name}` -> `{name,undoSteps:1}`. Reuses the interactive
    /// `"sheet\trename"` dispatch command (one `Structural` undo group), but
    /// validates the name FIRST — the dispatch command itself only checks
    /// non-empty, silently no-op'ing on an invalid one; mirrors xlsxy
    /// control.rs's `sheet_rename` validation.
    fn ctl_sheet_rename(&mut self, args: &json::Json) -> Result<String, String> {
        let si = self.ctl_sheet_arg_required(args)?;
        let name = args.get_str("name").ok_or("sheet.rename needs a 'name'")?;
        if name.is_empty() || name.contains(['[', ']', '*', '?', ':', '/', '\\']) {
            return Err("invalid sheet name".into());
        }
        self.dispatch(&format!("sheet\trename\t{si}\t{name}"));
        let mut out = String::from("{\"name\":");
        json::push_str(&mut out, name);
        out.push_str(",\"undoSteps\":1}");
        Ok(out)
    }

    /// `{at,count?,sheet?}` -> `{inserted|deleted,undoSteps:1}`. Reuses the
    /// interactive `"insrow"`/`"delrow"` dispatch commands (one `Structural`
    /// undo group each), temporarily swapping `active` to the target sheet
    /// since those commands target `self.active`.
    fn ctl_row_op(&mut self, args: &json::Json, insert: bool) -> Result<String, String> {
        let verb = if insert { "row.insert" } else { "row.delete" };
        let si = self.ctl_sheet_arg(args)?;
        let at = args
            .get_usize("at")
            .ok_or_else(|| format!("{verb} needs an 'at'"))?;
        let count = args.get_usize("count").unwrap_or(1);
        if count == 0 {
            return Err(format!("{verb}: 'count' must be at least 1"));
        }
        let prev_active = self.active;
        self.active = si;
        let cmd = if insert { "insrow" } else { "delrow" };
        self.dispatch(&format!("{cmd}\t{at}\t{count}"));
        self.active = prev_active;
        let key = if insert { "inserted" } else { "deleted" };
        Ok(format!("{{\"{key}\":{count},\"undoSteps\":1}}"))
    }

    /// `{at,count?,sheet?}` -> `{inserted|deleted,undoSteps:1}`. Same shape
    /// as [`Session::ctl_row_op`], for `"inscol"`/`"delcol"`.
    fn ctl_col_op(&mut self, args: &json::Json, insert: bool) -> Result<String, String> {
        let verb = if insert { "col.insert" } else { "col.delete" };
        let si = self.ctl_sheet_arg(args)?;
        let at = args
            .get_usize("at")
            .ok_or_else(|| format!("{verb} needs an 'at'"))?;
        let count = args.get_usize("count").unwrap_or(1);
        if count == 0 {
            return Err(format!("{verb}: 'count' must be at least 1"));
        }
        let prev_active = self.active;
        self.active = si;
        let cmd = if insert { "inscol" } else { "delcol" };
        self.dispatch(&format!("{cmd}\t{at}\t{count}"));
        self.active = prev_active;
        let key = if insert { "inserted" } else { "deleted" };
        Ok(format!("{{\"{key}\":{count},\"undoSteps\":1}}"))
    }

    /// `{query,text}` -> `{replaced,undoSteps:1}` — literal find/replace
    /// across every cell's input text, on EVERY sheet, as one `Structural`
    /// undo group (unconditionally — even a genuine no-match call still
    /// pushes one group, mirroring xlsxy control.rs's `wb_replace_all`
    /// exactly, which has no no-op guard). Mirrors the design decision
    /// flagged in Task 5's report: no `sheet?` arg, so this spans every
    /// sheet via `Session::structural`, not the single-sheet `apply`.
    fn ctl_wb_replace_all(&mut self, args: &json::Json) -> Result<String, String> {
        let query = args
            .get_str("query")
            .ok_or("wb.replace-all needs a 'query'")?;
        if query.is_empty() {
            return Err("empty query".into());
        }
        let text = args.get_str("text").ok_or("wb.replace-all needs 'text'")?;
        let mut replaced = 0usize;
        let today = self.engine.clock;
        self.structural(|wb| {
            let ctx = gridcore::entry::entry_ctx(wb, today);
            for sheet in &mut wb.sheets {
                let changes =
                    gridcore::edit::replace_all_in_sheet(sheet, &mut wb.styles, &ctx, query, text);
                replaced += changes.len();
                for (r, c, nc) in changes {
                    sheet.set_cell(r, c, nc);
                }
            }
        });
        Ok(format!("{{\"replaced\":{replaced},\"undoSteps\":1}}"))
    }

    /// `{text,name?}` -> `{sheet,name,rows,cols,undoSteps:0}` — plus the
    /// internal `inverse` (`sheet.remove` of the freshly-created sheet — an
    /// EXACT reversal, since the sheet never existed before). Always creates
    /// a NEW sheet (never overwrites); clears existing undo history, same
    /// package-parts reasoning as `sheet.remove`. Mirrors xlsxy control.rs's
    /// `sheet_import_csv`.
    fn ctl_sheet_import_csv(&mut self, args: &json::Json) -> Result<String, String> {
        let text = args
            .get_str("text")
            .ok_or("sheet.import-csv needs 'text'")?;
        let frame = Frame::from_csv(text);
        let requested = args.get_str("name").unwrap_or("Sheet");
        let name = ctl_unique_sheet_name(&self.pkg.workbook, requested);
        let idx = self.pkg.add_sheet(&name);
        frame.write_to_sheet(&mut self.pkg.workbook.sheets[idx]);
        let (rows, cols) = self.pkg.workbook.sheets[idx].used_size();
        self.undo.clear();
        self.redo.clear();
        self.rebuild_engine();
        self.dirty = true;
        let mut out = String::from("{\"sheet\":");
        out.push_str(&idx.to_string());
        out.push_str(",\"name\":");
        json::push_str(&mut out, &name);
        out.push_str(",\"rows\":");
        out.push_str(&rows.to_string());
        out.push_str(",\"cols\":");
        out.push_str(&cols.to_string());
        out.push_str(",\"inverse\":{\"verb\":\"sheet.remove\",\"args\":{\"sheet\":");
        out.push_str(&idx.to_string());
        out.push_str("}},\"undoSteps\":0}");
        Ok(out)
    }
}

/// Append a cell value as JSON: `Empty` -> `null`, `Number` -> a JSON number,
/// `Text`/`Error` -> a JSON string, `Bool` -> `true`/`false`.
fn ctl_push_value(out: &mut String, v: &CellValue) {
    match v {
        CellValue::Empty => out.push_str("null"),
        CellValue::Number(n) => json::push_num(out, *n),
        CellValue::Text(s) => json::push_str(out, s),
        CellValue::Bool(true) => out.push_str("true"),
        CellValue::Bool(false) => out.push_str("false"),
        CellValue::Error(e) => json::push_str(out, e),
    }
}

/// Serialize a `comment.replace-thread` inverse request for `(sheet, cref)`
/// carrying `messages` (author/text pairs, in order). The internal `inverse`
/// of comment.add/remove/replace-thread — never emitted on the wire (Task 7's
/// `CtlServer` strips `inverse`), so its INTERNAL-ONLY verb never leaks.
fn ctl_replace_thread_inverse(sheet: usize, cref: &str, messages: &[(String, String)]) -> String {
    let mut out = String::from("{\"verb\":\"comment.replace-thread\",\"args\":{\"sheet\":");
    out.push_str(&sheet.to_string());
    out.push_str(",\"ref\":");
    json::push_str(&mut out, cref);
    out.push_str(",\"messages\":[");
    for (i, (author, text)) in messages.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"author\":");
        json::push_str(&mut out, author);
        out.push_str(",\"text\":");
        json::push_str(&mut out, text);
        out.push('}');
    }
    out.push_str("]}}");
    out
}

/// Parse the `ref` arg (`"B4"`) into (row, col).
fn ctl_ref_arg(args: &json::Json) -> Result<(u32, u32), String> {
    let r = args
        .get_str("ref")
        .ok_or("needs a cell 'ref' like \"B4\"")?;
    parse_cell_name(r.trim()).ok_or_else(|| format!("bad cell ref '{r}'"))
}

/// Parse `"A1:C10"` (or a single `"B4"`) into (r1, c1, r2, c2), normalized.
fn ctl_parse_range(s: &str) -> Result<(u32, u32, u32, u32), String> {
    let t = s.trim();
    if let Some((r1, c1, r2, c2)) = parse_range_name(t) {
        return Ok((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)));
    }
    if let Some((r, c)) = parse_cell_name(t) {
        return Ok((r, c, r, c));
    }
    Err(format!("bad range '{s}' (use A1 or A1:C10)"))
}

/// `cell.format`'s `patch` object as wire key/value STRINGS —
/// `gridcore::format::FormatPatch::parse` does the actual key/value
/// validation and stays JSON-free, so scalar-to-string stringification lives
/// here (`true`/`false` for booleans, the raw text for strings/numbers; a
/// non-scalar value stringifies to `""`, which then fails `FormatPatch`'s
/// own per-key validation with a clear message rather than panicking or
/// silently no-opping). Key order is preserved from the request. Mirrors
/// xlsxy control.rs's `patch_pairs` exactly, including its error string for
/// a non-object `patch`.
fn ctl_patch_pairs(patch: &json::Json) -> Result<Vec<(String, String)>, String> {
    let json::Json::Obj(pairs) = patch else {
        return Err("cell.format needs a 'patch' object".to_string());
    };
    Ok(pairs
        .iter()
        .map(|(k, v)| {
            let text = match v {
                json::Json::Str(s) => s.clone(),
                json::Json::Bool(b) => b.to_string(),
                json::Json::Num(n) => n.to_string(),
                json::Json::Null | json::Json::Arr(_) | json::Json::Obj(_) => String::new(),
            };
            (k.clone(), text)
        })
        .collect())
}

/// Resolve `col.width`'s `col` arg — a column letter (`"C"`), a 0-based
/// numeric index, or a digit-string index (`"5"`, so the schema's "letter or
/// 0-based index" description is truthful for schema-conforming string
/// inputs too) — mirroring [`Session::ctl_sheet_arg`]'s index-or-name
/// flexibility. Mirrors xlsxy control.rs's `col_arg` exactly, including its
/// error strings. Every arm is bound to `gridcore::sheet::MAX_COLS`, the same
/// bound [`parse_col`] already applies to the letter arm: an out-of-range
/// numeric index used to sail straight through into a saved `.xlsx`'s
/// `ColDef`, which Excel then refuses to open without a "needs repair"
/// prompt.
fn ctl_col_arg(args: &json::Json) -> Result<u32, String> {
    match args.get("col") {
        Some(json::Json::Num(_)) => {
            let c = args
                .get_usize("col")
                .map(|c| c as u32)
                .ok_or_else(|| "bad 'col' index".to_string())?;
            if c >= gridcore::sheet::MAX_COLS {
                return Err(format!("bad column '{c}'"));
            }
            Ok(c)
        }
        Some(json::Json::Str(s)) => {
            let t = s.trim();
            if let Some((col, used)) = parse_col(t) {
                if used == t.len() {
                    return Ok(col);
                }
            } else if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(c) = t.parse::<u32>() {
                    if c < gridcore::sheet::MAX_COLS {
                        return Ok(c);
                    }
                }
            }
            Err(format!("bad column '{s}'"))
        }
        _ => Err("col.width needs a 'col' (letter or 0-based index)".to_string()),
    }
}

/// Serialize `col.width`'s internal bucket-B `inverse`: another `col.width`
/// call, over the same `(sheet, col)`, carrying the width to restore. Used
/// both for the forward call's inverse (the PRIOR width) and, when the host
/// applies that inverse, for ITS OWN reply's inverse (the width just
/// replaced) — so the chain alternates prior/current indefinitely and redo
/// keeps working.
fn ctl_col_width_inverse(sheet: usize, col: u32, width: f64) -> String {
    let mut out = String::from("{\"verb\":\"col.width\",\"args\":{\"col\":");
    out.push_str(&col.to_string());
    out.push_str(",\"width\":");
    json::push_num(&mut out, width);
    out.push_str(",\"sheet\":");
    out.push_str(&sheet.to_string());
    out.push_str("}}");
    out
}

/// The `cell.get` read-back `format` object for style index `style`: only
/// the `cell.format` patch keys whose value differs from the default style,
/// via `gridcore::format::xf_format_fields`; `None` for an unstyled cell (no
/// `format` key on the wire at all). Mirrors xlsxy control.rs's
/// `format_json`.
fn ctl_format_json(styles: &Styles, style: u32) -> Option<String> {
    let xf = styles.xf(style);
    let fields = xf_format_fields(&xf);
    if fields.is_empty() {
        return None;
    }
    let mut out = String::from("{");
    for (i, (k, v)) in fields.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json::push_str(&mut out, k);
        out.push(':');
        match v {
            FormatValue::Str(s) => json::push_str(&mut out, &s),
            FormatValue::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        }
    }
    out.push('}');
    Some(out)
}

/// A sheet name derived from `base`, deduplicated against existing sheet
/// names by appending " 2", " 3", … — same scheme xlsxy control.rs's
/// `unique_sheet_name` uses.
fn ctl_unique_sheet_name(wb: &gridcore::sheet::Workbook, base: &str) -> String {
    if wb.sheet_index(base).is_none() {
        return base.to_string();
    }
    let mut n = 1;
    loop {
        n += 1;
        let candidate = format!("{base} {n}");
        if wb.sheet_index(&candidate).is_none() {
            return candidate;
        }
    }
}

/// An array of header-name strings (`rows`/`cols`), defaulting to empty when
/// the key is absent.
fn ctl_names_arg(args: &json::Json, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(json::Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A default pivot-sheet name: `Pivot1`, `Pivot2`, … — unique among
/// existing sheet names. Distinct pattern from `ctl_unique_sheet_name`'s
/// "Sheet"/"Sheet 2" (no space before the number) — Wave-3's convention for
/// agent-created pivot sheets. Mirrors xlsxy control.rs's
/// `unique_pivot_name`.
fn ctl_unique_pivot_name(wb: &gridcore::sheet::Workbook) -> String {
    let mut n = 1;
    loop {
        let candidate = format!("Pivot{n}");
        if wb.sheet_index(&candidate).is_none() {
            return candidate;
        }
        n += 1;
    }
}

/// One `{col, agg}` pair from a pivot verb's `values` array — shared by
/// `sheet.pivot` and `pivot.create`, `verb` names the caller in errors so
/// parity between the two stays honest about which verb actually failed
/// (mirrors the same fix in xlsxy control.rs's `parse_measure_arg`).
fn ctl_parse_measure_arg(verb: &str, v: &json::Json) -> Result<(String, Agg), String> {
    let col = v
        .get_str("col")
        .ok_or_else(|| format!("{verb}: each value needs a 'col'"))?
        .to_string();
    let agg_s = v
        .get_str("agg")
        .ok_or_else(|| format!("{verb}: each value needs an 'agg'"))?;
    let agg = Agg::from_verb_name(agg_s).ok_or_else(|| format!("{verb}: unknown agg '{agg_s}'"))?;
    Ok((col, agg))
}

/// The typed result of a formula value, as JSON.
fn ctl_push_formula_value(out: &mut String, v: &Value) {
    match v {
        Value::Empty => out.push_str("null"),
        Value::Num(n) => json::push_num(out, *n),
        Value::Str(s) => json::push_str(out, s),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Err(e) => json::push_str(out, e.code()),
    }
}

/// The general-format display text of a formula value.
fn ctl_formula_value_text(v: &Value) -> String {
    match v {
        Value::Empty => String::new(),
        Value::Num(n) => fmt_general(*n),
        Value::Str(s) => s.clone(),
        Value::Bool(true) => "TRUE".to_string(),
        Value::Bool(false) => "FALSE".to_string(),
        Value::Err(e) => e.code().to_string(),
    }
}

/// Splice `"ok":true` into a ctl verb's result object string (`{…}`),
/// completing the success envelope.
fn ctl_ok(body: String) -> String {
    let mut s = body;
    s.pop(); // trailing '}'
    s.push_str(",\"ok\":true}");
    s
}

/// The ctl failure envelope: `{"ok":false,"error":"…"}`.
fn ctl_err(msg: &str) -> String {
    let mut out = String::from("{\"ok\":false,\"error\":");
    json::push_str(&mut out, msg);
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcore::sheet::Cell;
    use gridcore::xlsx::{new_xlsx, save_xlsx};

    /// A real .xlsx (one sheet, a few cells incl. a formula) built through the
    /// package layer, so tests exercise the same load path the webview uses.
    fn sample_xlsx() -> Vec<u8> {
        let mut pkg = new_xlsx();
        let sh = &mut pkg.workbook.sheets[0];
        sh.set_cell(0, 0, Cell::text("Item"));
        sh.set_cell(0, 1, Cell::text("Price"));
        sh.set_cell(1, 0, Cell::text("Apple"));
        sh.set_cell(1, 1, Cell::number(1.25));
        sh.set_cell(2, 0, Cell::text("Pear"));
        sh.set_cell(2, 1, Cell::number(2.5));
        sh.set_cell(3, 1, Cell::formula("SUM(B1:B3)"));
        save_xlsx(&pkg)
    }

    #[test]
    fn opens_and_renders_viewport() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");
        assert!(v.contains("Apple"), "{v}");
        assert!(v.contains("3.75"), "formula not recalculated: {v}");
        assert!(v.contains("\"dirty\":false"), "{v}");
        assert!(v.contains("\"cur\":{\"ref\":\"A1\""), "{v}");
    }

    #[test]
    fn viewport_clips_to_window() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("view\t0\t0\t0\t2\t1"); // rows 0..2, col 0 only
        let v = s.view_json(None);
        assert!(v.contains("Item") && v.contains("Apple"), "{v}");
        assert!(!v.contains("Price"), "col 1 must be clipped: {v}");
        assert!(!v.contains("Pear"), "row 2 must be clipped: {v}");
        // dims still reports the full used extent, not the window
        assert!(v.contains("\"dims\":{\"rows\":4,\"cols\":2}"), "{v}");
    }

    #[test]
    fn open_rejects_garbage() {
        assert!(Session::open(b"not an xlsx").is_none());
    }

    #[test]
    fn set_recalculates_dependents_and_marks_dirty() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t1\t1\t10"); // B2: 1.25 -> 10
        let v = s.view_json(None);
        assert!(v.contains("12.5"), "SUM must update: {v}");
        assert!(v.contains("\"dirty\":true"), "{v}");
    }

    #[test]
    fn set_formula_validates_and_reports_errors() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t5\t0\t=SUM(");
        let v = s.view_json(None);
        assert!(
            v.contains("\"err\":"),
            "invalid formula must surface err: {v}"
        );
        // and the cell must not have been written
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"\""), "cell should stay empty: {v}");
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t1\t1\t10");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(v.contains("3.75"), "undo must restore SUM: {v}");
        s.dispatch("redo");
        let v = s.view_json(None);
        assert!(v.contains("12.5"), "redo must reapply: {v}");
    }

    #[test]
    fn undo_restores_a_legacy_formula_as_it_was() {
        // #724: undo puts the snapshot back as it was (restore_cell), not as a
        // typed formula — a loaded legacy formula must not come back as a
        // dynamic array that saves with a `cm`.
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.pkg.workbook.sheets[0].set_cell(9, 5, Cell::formula("B2:B4*2"));
        s.rebuild_engine();
        let before = s.pkg.workbook.sheets[0].cell(9, 5).cloned().unwrap();
        s.dispatch("set	9	5	7");
        s.dispatch("undo");
        let f10 = s.pkg.workbook.sheets[0].cell(9, 5).unwrap();
        assert_eq!(f10, &before);
        assert!(!f10.is_modern() && !f10.is_dynamic() && f10.spill.is_none());
    }

    #[test]
    fn clear_range_clears_as_one_undo_group() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("clear\t0\t0\t2\t1"); // wipe rows 0..=2
        let v = s.view_json(None);
        assert!(!v.contains("Apple"), "{v}");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(
            v.contains("Apple") && v.contains("3.75"),
            "one undo restores all: {v}"
        );
    }

    #[test]
    fn select_extends_selection_and_moves_cur() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t1\t0\t2\t1");
        let v = s.view_json(None);
        assert!(
            v.contains("\"sel\":{\"r\":1,\"c\":0,\"r2\":2,\"c2\":1}"),
            "{v}"
        );
        assert!(v.contains("\"ref\":\"A2\""), "{v}");
    }

    #[test]
    fn insert_row_rewrites_references_and_undoes() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("insrow\t1\t1"); // push data rows down: SUM(B1:B3) -> SUM(B1:B4)
        s.dispatch("select\t4\t1");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=SUM(B1:B4)\""),
            "refs must rewrite: {v}"
        );
        assert!(v.contains("3.75"), "total unchanged: {v}");
        s.dispatch("undo");
        s.dispatch("select\t3\t1");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=SUM(B1:B3)\""),
            "undo restores refs: {v}"
        );
    }

    #[test]
    fn delete_col_and_undo() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("delcol\t0\t1"); // drop the Item column; Price shifts to col 0
        let v = s.view_json(None);
        assert!(!v.contains("Apple"), "{v}");
        assert!(v.contains("3.75"), "sum column survives: {v}");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(v.contains("Apple"), "undo restores: {v}");
    }

    #[test]
    fn copy_returns_tsv_and_paste_round_trips() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t0\t0\t1\t1");
        let tsv = s.dispatch("copy").expect("copy returns tsv");
        assert_eq!(tsv, "Item\tPrice\nApple\t1.25");
        s.dispatch("paste\t5\t0\tItem\tPrice\nApple\t1.25");
        s.dispatch("select\t6\t1");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"1.25\""), "pasted number: {v}");
        s.dispatch("undo");
        s.dispatch("select\t6\t1");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"\""), "paste is one undo group: {v}");
    }

    #[test]
    fn pasted_text_is_read_like_typed_entry() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("paste\t8\t0\t1/15/2024\t$5\t'007");
        let wb = &s.pkg.workbook;
        let at = |c: u32| wb.sheets[0].cell(8, c).cloned().unwrap();
        let date = at(0);
        assert_eq!(date.value, CellValue::Number(45_306.0));
        assert_eq!(wb.styles.xf(date.style).code.as_deref(), Some("m/d/yyyy"));
        assert_eq!(at(1).value, CellValue::Number(5.0));
        assert_eq!(at(2).value, CellValue::Text("007".into()));
        assert!(wb.styles.xf(at(2).style).quote_prefix);
    }

    #[test]
    fn the_editor_seeds_every_digit_and_a_percent_cell_its_percent() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.pkg.workbook.sheets[0].set_cell(9, 0, Cell::number(0.1 + 0.2));
        let mut xf = gridcore::sheet::Xf::default();
        xf.set_code(Some("0%".into()));
        let pct = s.pkg.workbook.styles.intern(xf);
        s.pkg.workbook.sheets[0].set_cell(
            9,
            1,
            Cell {
                style: pct,
                ..Cell::number(1.5)
            },
        );
        assert_eq!(s.cell_seed(9, 0), "0.30000000000000004");
        assert_eq!(s.cell_seed(9, 1), "150%");
        // The seed re-enters as the same value.
        s.dispatch("set\t9\t1\t150%");
        let b10 = s.pkg.workbook.sheets[0].cell(9, 1).unwrap().value.clone();
        assert_eq!(b10, CellValue::Number(1.5));
    }

    #[test]
    fn a_general_number_is_fitted_to_its_column() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.pkg.workbook.sheets[0].set_cell(9, 0, Cell::number(123_456_789_012.0));
        let chars = s.pkg.workbook.sheets[0].col_width(0).floor() as usize;
        let want = gridcore::sheet::fmt_general_cell(123_456_789_012.0, chars);
        let v = s.view_json(None);
        assert!(v.contains(&format!("\"t\":\"{want}\"")), "{want}: {v}");
        assert!(!v.contains("\"t\":\"123456789012\""), "{v}");
        // A wide column still stops at General's 11 characters.
        s.pkg.workbook.sheets[0].set_col_width(0, 30.0);
        let v = s.view_json(None);
        assert!(v.contains("\"t\":\"1.23457E+11\""), "{v}");
        // The editor keeps every digit.
        assert_eq!(s.cell_seed(9, 0), "123456789012");
    }

    #[test]
    fn copy_preserves_formulas_as_source() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t3\t1");
        let tsv = s.dispatch("copy").expect("copy");
        assert_eq!(tsv, "=SUM(B1:B3)");
    }

    #[test]
    fn a_copy_pasted_back_is_the_same_cells_between_text_and_general() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let styles = &mut s.pkg.workbook.styles;
        let quoted = styles.intern(gridcore::sheet::Xf {
            quote_prefix: true,
            ..gridcore::sheet::Xf::default()
        });
        let mut text_xf = gridcore::sheet::Xf::default();
        text_xf.set_code(Some("@".into()));
        let text_fmt = styles.intern(text_xf);
        let sh = &mut s.pkg.workbook.sheets[0];
        let q007 = Cell {
            style: quoted,
            ..Cell::text("007")
        };
        let tabc = Cell {
            style: text_fmt,
            ..Cell::text("'abc")
        };
        sh.set_cell(10, 0, q007.clone());
        sh.set_cell(10, 1, tabc.clone());
        // Targets of the other kind.
        sh.set_cell(
            12,
            0,
            Cell {
                style: text_fmt,
                ..Cell::default()
            },
        );
        sh.set_cell(12, 1, Cell::text("x"));
        s.dispatch("select\t10\t0\t10\t1");
        let tsv = s.dispatch("copy").unwrap();
        // The host hands the text back with CRLF and a trailing line break.
        s.dispatch(&format!("paste\t12\t0\t{}\r\n", tsv.replace('\n', "\r\n")));
        let at = |s: &Session, r, c| s.pkg.workbook.sheets[0].cell(r, c).cloned().unwrap();
        assert_eq!(at(&s, 12, 0), q007);
        assert_eq!(at(&s, 12, 1), tabc);
        // Text from elsewhere is still read as typed into the target.
        s.dispatch("paste\t13\t0\t'abc");
        assert_eq!(at(&s, 13, 0).value, CellValue::Text("abc".into()));
    }

    #[test]
    fn a_copy_ending_in_an_empty_row_still_pastes_as_cells() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let sh = &mut s.pkg.workbook.sheets[0];
        sh.set_cell(20, 0, Cell::formula("B21"));
        sh.set_cell(25, 2, Cell::text("gone"));
        s.dispatch("select\t20\t0\t21\t0");
        let tsv = s.dispatch("copy").unwrap();
        assert!(tsv.ends_with('\n'), "{tsv:?}");
        for pasted in [
            tsv.clone(),
            tsv.trim_end().to_string(),
            tsv.replace('\n', "\r\n"),
        ] {
            s.dispatch(&format!("paste\t24\t2\t{pasted}"));
            let at = |r| {
                s.pkg.workbook.sheets[0]
                    .cell(r, 2)
                    .cloned()
                    .unwrap_or_default()
            };
            assert_eq!(at(24).formula.as_deref(), Some("D25"), "{pasted:?}");
            assert!(at(25).is_blank(), "{pasted:?}: the empty row pastes too");
            s.pkg.workbook.sheets[0].set_cell(25, 2, Cell::text("gone"));
        }
    }

    #[test]
    fn a_pasted_copy_moves_its_references_and_a_cut_keeps_them() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let f = |s: &Session, r, c| {
            s.pkg.workbook.sheets[0]
                .cell(r, c)
                .and_then(|c| c.formula.clone())
        };
        s.dispatch("select\t3\t1"); // =SUM(B1:B3)
        let tsv = s.dispatch("copy").unwrap();
        s.dispatch(&format!("paste\t3\t2\t{tsv}"));
        assert_eq!(f(&s, 3, 2).as_deref(), Some("SUM(C1:C3)"));
        // A cut records the cells before clearing them, and pastes them
        // unchanged without clearing the source again.
        s.dispatch("select\t3\t1");
        let tsv = s.dispatch("cut").unwrap();
        assert_eq!(f(&s, 3, 1), None, "the cut cleared its source");
        s.dispatch(&format!("paste\t5\t3\t{tsv}"));
        assert_eq!(f(&s, 5, 3).as_deref(), Some("SUM(B1:B3)"));
        // Pasted again, it is a copy: its references move.
        s.dispatch(&format!("paste\t5\t4\t{tsv}"));
        assert_eq!(f(&s, 5, 4).as_deref(), Some("SUM(E3:E5)"));
    }

    /// Column `c`'s values, rows 0..=2, on the first sheet.
    fn col3(s: &Session, c: u32) -> Vec<CellValue> {
        (0..3)
            .map(|r| {
                s.pkg.workbook.sheets[0]
                    .cell(r, c)
                    .map(|cl| cl.value.clone())
                    .unwrap_or(CellValue::Empty)
            })
            .collect()
    }

    #[test]
    fn paste_whole_spill_block_respills_through_undo_redo() {
        // #777: a copied spill block pastes as a spilling anchor, also on
        // redo; undo gives back what was there.
        let n = |v: f64| CellValue::Number(v);
        let seq = vec![n(1.0), n(2.0), n(3.0)];
        for old in [None, Some(["a", "b", "c"])] {
            let mut s = Session::open(&sample_xlsx()).expect("open");
            s.dispatch("set\t0\t4\t=SEQUENCE(3)"); // E1 spills E1:E3
            assert_eq!(col3(&s, 4), seq);
            if let Some(old) = old {
                for (r, v) in old.iter().enumerate() {
                    s.dispatch(&format!("set\t{r}\t6\t{v}"));
                }
            }
            let was = col3(&s, 6);
            s.dispatch("select\t0\t4\t2\t4");
            let tsv = s.dispatch("copy").unwrap();
            s.dispatch(&format!("paste\t0\t6\t{tsv}"));
            assert_eq!(col3(&s, 6), seq, "pasted over {old:?}");
            for round in 0..2 {
                s.dispatch("undo");
                assert_eq!(col3(&s, 6), was, "undo {round} over {old:?}");
                s.dispatch("redo");
                assert_eq!(col3(&s, 6), seq, "redo {round} over {old:?}");
                let g1 = s.pkg.workbook.sheets[0].cell(0, 6).unwrap();
                assert_eq!(g1.spill, Some((3, 1)));
            }
        }
    }

    #[test]
    fn paste_single_anchor_over_occupied_spills_error_through_undo_redo() {
        // #777 / #725: a pasted anchor whose spill area is occupied is
        // #SPILL! and keeps the occupant.
        let n = |v: f64| CellValue::Number(v);
        let keep = CellValue::Text("keep".into());
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t0\t4\t=SEQUENCE(3)");
        s.dispatch("set\t1\t6\tkeep");
        s.dispatch("select\t0\t4");
        let tsv = s.dispatch("copy").unwrap();
        s.dispatch(&format!("paste\t0\t6\t{tsv}"));
        let after = vec![
            CellValue::Error("#SPILL!".into()),
            keep.clone(),
            CellValue::Empty,
        ];
        assert_eq!(col3(&s, 6), after);
        s.dispatch("undo");
        assert_eq!(col3(&s, 6), vec![CellValue::Empty, keep, CellValue::Empty]);
        s.dispatch("redo");
        assert_eq!(col3(&s, 6), after);
        // Over empty cells it spills; undo empties them; redo re-spills.
        s.dispatch(&format!("paste\t0\t9\t{tsv}"));
        let seq = vec![n(1.0), n(2.0), n(3.0)];
        assert_eq!(col3(&s, 9), seq);
        s.dispatch("undo");
        assert_eq!(col3(&s, 9), vec![CellValue::Empty; 3]);
        s.dispatch("redo");
        assert_eq!(col3(&s, 9), seq);
    }

    #[test]
    fn undo_redo_around_a_live_spill_keeps_it_spilling() {
        // #777 r1: spill output edited without its anchor is snapshotted as
        // the blank its anchor re-spills over, not as a plain value.
        let n = |v: f64| CellValue::Number(v);
        let seq = vec![n(1.0), n(2.0), n(3.0)];
        let blocked = vec![
            CellValue::Error("#SPILL!".into()),
            CellValue::Text("x".into()),
            CellValue::Empty,
        ];
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t0\t4\t=SEQUENCE(3)"); // E1 spills E1:E3
        // Delete E2:E3: a no-op, and so are undo and redo.
        s.dispatch("clear\t1\t4\t2\t4");
        for step in ["clear", "undo", "redo", "undo"] {
            if step != "clear" {
                s.dispatch(step);
            }
            assert_eq!(col3(&s, 4), seq, "{step}");
            let e1 = s.pkg.workbook.sheets[0].cell(0, 4).unwrap();
            assert_eq!(e1.spill, Some((3, 1)), "{step}");
        }
        // Type into a spill child: #SPILL!; undo re-spills; redo blocks.
        s.dispatch("set\t1\t4\tx");
        assert_eq!(col3(&s, 4), blocked);
        s.dispatch("undo");
        assert_eq!(col3(&s, 4), seq);
        s.dispatch("redo");
        assert_eq!(col3(&s, 4), blocked);
        // Delete the blocker: it spills; undo brings it back; redo spills.
        s.dispatch("clear\t1\t4\t1\t4");
        assert_eq!(col3(&s, 4), seq);
        s.dispatch("undo");
        assert_eq!(col3(&s, 4), blocked);
        s.dispatch("redo");
        assert_eq!(col3(&s, 4), seq);
    }

    #[test]
    fn undoing_an_overwrite_of_a_frozen_anchor_restores_its_block() {
        // #837: typing over a frozen anchor clears its cached block; undo
        // puts the block back, extent and saved `ref` too.
        let n = |v: f64| CellValue::Number(v);
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        let typed = vec![n(5.0), CellValue::Empty, CellValue::Empty];
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.set_cell(0, 0, Cell::number(1.0));
        sheet.set_cell(
            0,
            4,
            Cell {
                value: n(7.0),
                formula: Some("PIVOTBY(A1,4)".into()),
                f_attrs: Some("t=\"array\" ref=\"E1:E3\"".into()),
                spill: Some((3, 1)),
                ..Cell::default()
            },
        );
        sheet.set_cell(1, 4, Cell::number(8.0));
        sheet.set_cell(2, 4, Cell::number(9.0));
        let mut s = Session::open(&save_xlsx(&pkg)).expect("open");
        assert_eq!(col3(&s, 4), cached, "loaded");
        let f_attrs = s.pkg.workbook.sheets[0].cell(0, 4).unwrap().f_attrs.clone();
        assert!(f_attrs.is_some());
        s.dispatch("set\t0\t4\t5");
        assert_eq!(col3(&s, 4), typed);
        for round in 0..2 {
            s.dispatch("undo");
            assert_eq!(col3(&s, 4), cached, "undo {round}");
            let e1 = s.pkg.workbook.sheets[0].cell(0, 4).unwrap();
            assert_eq!(e1.spill, Some((3, 1)), "undo {round}");
            assert_eq!(e1.f_attrs, f_attrs, "undo {round}");
            if round == 0 {
                s.dispatch("redo");
                assert_eq!(col3(&s, 4), typed, "redo");
            }
        }
        let re = Session::open(&s.save()).expect("reopen");
        assert_eq!(col3(&re, 4), cached, "saved");
        let ws =
            String::from_utf8_lossy(re.pkg.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        assert!(ws.contains(r#"<f t="array" ref="E1:E3">"#), "{ws}");
    }

    #[test]
    fn recommitting_a_frozen_anchor_keeps_its_block() {
        // #840: grid.js commits an unchanged editor (F2, Enter), so the same
        // formula comes back; a frozen anchor keeps its cached block, extent
        // and saved `ref`, through undo and redo.
        let n = |v: f64| CellValue::Number(v);
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.set_cell(0, 0, Cell::number(1.0));
        sheet.set_cell(
            0,
            4,
            Cell {
                value: n(7.0),
                formula: Some("PIVOTBY(A1,4)".into()),
                f_attrs: Some("t=\"array\" ref=\"E1:E3\"".into()),
                spill: Some((3, 1)),
                ..Cell::default()
            },
        );
        sheet.set_cell(1, 4, Cell::number(8.0));
        sheet.set_cell(2, 4, Cell::number(9.0));
        let mut s = Session::open(&save_xlsx(&pkg)).expect("open");
        assert_eq!(col3(&s, 4), cached, "loaded");
        // The editor's seed, as grid.js commits it back.
        let seed = s.cell_seed(0, 4);
        assert_eq!(seed, "=_xlfn.PIVOTBY(A1,4)");
        s.dispatch(&format!("set	0	4	{seed}"));
        for step in ["set", "undo", "redo"] {
            if step != "set" {
                s.dispatch(step);
            }
            assert_eq!(col3(&s, 4), cached, "{step}");
            let e1 = s.pkg.workbook.sheets[0].cell(0, 4).unwrap();
            assert_eq!(e1.spill, Some((3, 1)), "{step}");
        }
        let re = Session::open(&s.save()).expect("reopen");
        assert_eq!(col3(&re, 4), cached, "saved");
        let ws =
            String::from_utf8_lossy(re.pkg.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        assert!(ws.contains(r#"<f t="array" ref="E1:E3">"#), "{ws}");
    }

    #[test]
    fn undoing_typing_into_a_frozen_block_restores_its_extent() {
        // #837 r2: a value typed into a frozen block drops its anchor's
        // extent; undo puts the extent back, so the block saves whole. One
        // cell, then every cell but the anchor.
        let n = |v: f64| CellValue::Number(v);
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        let edits = [
            (vec!["set	1	4	5"], vec![n(7.0), n(5.0), n(9.0)]),
            (
                vec![
                    "select	1	4	2	4",
                    "paste	1	4	5
6",
                ],
                vec![n(7.0), n(5.0), n(6.0)],
            ),
        ];
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.set_cell(0, 0, Cell::number(1.0));
        sheet.set_cell(
            0,
            4,
            Cell {
                value: n(7.0),
                formula: Some("PIVOTBY(A1,4)".into()),
                f_attrs: Some("t=\"array\" ref=\"E1:E3\"".into()),
                spill: Some((3, 1)),
                ..Cell::default()
            },
        );
        sheet.set_cell(1, 4, Cell::number(8.0));
        sheet.set_cell(2, 4, Cell::number(9.0));
        let file = save_xlsx(&pkg);
        for (cmds, typed) in edits {
            let mut s = Session::open(&file).expect("open");
            let at = cmds.join(" ");
            let extent = |s: &Session| s.pkg.workbook.sheets[0].cell(0, 4).unwrap().spill;
            for cmd in &cmds {
                s.dispatch(cmd);
            }
            assert_eq!(col3(&s, 4), typed, "{at}");
            for round in 0..2 {
                s.dispatch("undo");
                assert_eq!(col3(&s, 4), cached, "undo {round}, {at}");
                assert_eq!(extent(&s), Some((3, 1)), "undo {round}, {at}");
                if round == 0 {
                    s.dispatch("redo");
                    assert_eq!(col3(&s, 4), typed, "redo, {at}");
                }
            }
            let re = Session::open(&s.save()).expect("reopen");
            assert_eq!(col3(&re, 4), cached, "saved, {at}");
            let ws = String::from_utf8_lossy(re.pkg.part("xl/worksheets/sheet1.xml").unwrap())
                .into_owned();
            assert!(ws.contains(r#"<f t="array" ref="E1:E3">"#), "{at}: {ws}");
        }
    }

    #[test]
    fn undoing_an_overwrite_of_a_live_anchor_re_spills_it() {
        // #837 guard: a live anchor's block is not recorded; undo re-spills
        // it from its formula.
        let n = |v: f64| CellValue::Number(v);
        let seq = vec![n(1.0), n(2.0), n(3.0)];
        let x = vec![
            CellValue::Text("x".into()),
            CellValue::Empty,
            CellValue::Empty,
        ];
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t0\t4\t=SEQUENCE(3)");
        s.dispatch("set\t0\t4\tx");
        assert_eq!(col3(&s, 4), x);
        s.dispatch("undo");
        assert_eq!(col3(&s, 4), seq);
        let e1 = s.pkg.workbook.sheets[0].cell(0, 4).unwrap();
        assert_eq!(e1.spill, Some((3, 1)));
        s.dispatch("redo");
        assert_eq!(col3(&s, 4), x);
    }

    #[test]
    fn cut_copies_then_clears() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t1\t0");
        let tsv = s.dispatch("cut").expect("cut");
        assert_eq!(tsv, "Apple");
        let v = s.view_json(None);
        assert!(!v.contains("Apple"), "{v}");
    }

    /// A session on a file with A1:A3 = 1, 2, 3 and a legacy CSE block
    /// `{=A1:A3*2}` over D1:D3, spilling 2, 4, 6.
    fn session_with_spilling_cse() -> Session {
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        for r in 0..3u32 {
            sheet.set_cell(r, 0, Cell::number(f64::from(r + 1)));
            sheet.set_cell(r, 3, Cell::number(f64::from(2 * (r + 1))));
        }
        sheet.set_cell(
            0,
            3,
            Cell {
                value: CellValue::Number(2.0),
                formula: Some("A1:A3*2".into()),
                f_attrs: Some(" t=\"array\" ref=\"D1:D3\"".into()),
                spill: Some((3, 1)),
                ..Cell::default()
            },
        );
        let s = Session::open(&save_xlsx(&pkg)).expect("open");
        assert_cse_spills(&s, "loaded");
        s
    }

    /// D1 is the CSE block over D1:D3, spilling 2, 4, 6.
    fn assert_cse_spills(s: &Session, when: &str) {
        let sheet = &s.pkg.workbook.sheets[0];
        let d1 = sheet.cell(0, 3).unwrap();
        assert_eq!(d1.spill, Some((3, 1)), "{when}: {:?}", d1.value);
        assert_eq!(
            d1.f_attrs.as_deref(),
            Some(" t=\"array\" ref=\"D1:D3\""),
            "{when}"
        );
        for r in 0..3u32 {
            assert_eq!(
                sheet.cell(r, 3).unwrap().value,
                CellValue::Number(f64::from(2 * (r + 1))),
                "{when}: D{}",
                r + 1
            );
        }
    }

    #[test]
    fn a_spilling_cse_block_pasted_in_place_still_spills() {
        // #825 AC4: copy (or cut) D1:D3 and paste it back at D1. Written cell
        // by cell, the 4 pasted into D2 blocked D1 (#SPILL!). Undo puts back
        // exactly what was there, and redo the paste, both still spilling.
        for verb in ["copy", "cut"] {
            let mut s = session_with_spilling_cse();
            s.dispatch("select\t0\t3\t2\t3"); // D1:D3
            let tsv = s.dispatch(verb).unwrap();
            let before = s.pkg.workbook.sheets[0].cells.clone();
            s.dispatch(&format!("paste\t0\t3\t{tsv}"));
            assert_cse_spills(&s, &format!("{verb}: paste"));
            let after = s.pkg.workbook.sheets[0].cells.clone();
            s.dispatch("undo");
            assert_eq!(s.pkg.workbook.sheets[0].cells, before, "{verb}: undo");
            s.dispatch("redo");
            assert_cse_spills(&s, &format!("{verb}: redo"));
            assert_eq!(s.pkg.workbook.sheets[0].cells, after, "{verb}: redo");
        }
    }

    #[test]
    fn an_edit_to_part_of_a_cse_block_is_refused_whole() {
        // r7 M2: typing, clearing, cutting or pasting into part of the block
        // changes nothing, records no undo group, leaves the session clean
        // and says why, as Excel does.
        let refused =
            |s: &mut Session, cmd: &str, before: &std::collections::BTreeMap<(u32, u32), Cell>| {
                assert_eq!(s.dispatch(cmd), None, "{cmd}");
                assert_eq!(&s.pkg.workbook.sheets[0].cells, before, "{cmd}");
                assert!(s.undo.is_empty(), "{cmd}");
                assert!(!s.dirty, "{cmd}");
                assert_eq!(s.err.take().as_deref(), Some(PART_OF_ARRAY), "{cmd}");
                assert_cse_spills(s, cmd);
            };
        let mut s = session_with_spilling_cse();
        s.pkg.workbook.sheets[0].set_cell(4, 0, Cell::number(7.0));
        s.pkg.workbook.sheets[0].set_cell(5, 0, Cell::number(8.0));
        let before = s.pkg.workbook.sheets[0].cells.clone();
        refused(&mut s, "set\t1\t3\t9", &before);
        refused(&mut s, "clear\t1\t3\t2\t3", &before);
        s.dispatch("select\t1\t3\t2\t3"); // D2:D3
        refused(&mut s, "cut", &before);
        // A copied column of two pasted at D2: refused whole, D4 too.
        s.dispatch("select\t4\t0\t5\t0"); // A5:A6
        let tsv = s.dispatch("copy").unwrap();
        refused(&mut s, &format!("paste\t1\t3\t{tsv}"), &before);
        // Its anchor taken too, the block goes.
        s.dispatch("clear\t0\t3\t2\t3");
        assert!(s.err.is_none());
        assert_eq!(s.undo.len(), 1);
        assert!(
            s.pkg.workbook.sheets[0]
                .cell(1, 3)
                .is_none_or(|c| c.value.is_empty())
        );
    }

    #[test]
    fn a_group_that_blocks_its_own_spill_undoes_and_redoes_exactly() {
        // #825 AC9: one edit types a spilling D1 and then a 5 into D2, which
        // blocks it. Snapshotted cell by cell, D2's before was D1's spilled
        // 4, which undo left behind.
        let mut s = Session::open(&save_xlsx(&new_xlsx())).expect("open");
        s.apply(
            (0..3)
                .map(|r| (r, 0, Cell::number(f64::from(r + 1))))
                .collect(),
        );
        let before = s.pkg.workbook.sheets[0].cells.clone();
        s.apply(vec![
            (0, 3, Cell::formula("A1:A3*2")),
            (1, 3, Cell::number(5.0)),
        ]);
        let blocked = |s: &Session, when: &str| {
            let sheet = &s.pkg.workbook.sheets[0];
            let d1 = sheet.cell(0, 3).unwrap();
            assert_eq!(d1.value, CellValue::Error("#SPILL!".into()), "{when}");
            assert_eq!(d1.spill, None, "{when}");
            assert_eq!(
                sheet.cell(1, 3).unwrap().value,
                CellValue::Number(5.0),
                "{when}"
            );
        };
        blocked(&s, "edit");
        let after = s.pkg.workbook.sheets[0].cells.clone();
        s.dispatch("undo");
        assert_eq!(s.pkg.workbook.sheets[0].cells, before, "undo");
        s.dispatch("redo");
        blocked(&s, "redo");
        assert_eq!(s.pkg.workbook.sheets[0].cells, after, "redo");
    }

    /// A session with `=SEQUENCE(3)` typed into D1 (spilling D1:D3).
    fn session_with_spill() -> Session {
        let mut s = Session::open(&save_xlsx(&new_xlsx())).expect("open");
        s.dispatch("set\t0\t3\t=SEQUENCE(3)");
        assert_eq!(spill_at_d1(&s), Some((3, 1)));
        s
    }

    fn spill_at_d1(s: &Session) -> Option<(u32, u32)> {
        s.pkg.workbook.sheets[0].cell(0, 3).and_then(|cl| cl.spill)
    }

    /// D1 still spills over D1:D3 (values 1..3), each cell (not) bold.
    fn assert_spill_bold(s: &Session, bold: bool, when: &str) {
        assert_eq!(spill_at_d1(s), Some((3, 1)), "{when}");
        for r in 0..3u32 {
            let cell = s.pkg.workbook.sheets[0].cell(r, 3).unwrap();
            assert_eq!(cell.value, CellValue::Number(f64::from(r + 1)), "{when}");
            assert_eq!(s.pkg.workbook.styles.xf(cell.style).bold, bold, "{when}");
        }
    }

    #[test]
    fn fmt_bold_on_a_spill_block_keeps_the_spill_through_undo_redo() {
        // #784: formatting a spilled block left D1 #SPILL!.
        let mut s = session_with_spill();
        s.dispatch("select\t0\t3\t2\t3"); // D1:D3
        s.dispatch("fmt\tbold\ttoggle");
        assert_spill_bold(&s, true, "bold");
        s.dispatch("undo");
        assert_spill_bold(&s, false, "undo");
        s.dispatch("redo");
        assert_spill_bold(&s, true, "redo");
    }

    #[test]
    fn ctl_cell_format_on_a_spill_block_keeps_the_spill_through_undo_redo() {
        let mut s = session_with_spill();
        let r = s.ctl(r#"{"verb":"cell.format","args":{"range":"D1:D3","patch":{"bold":true}}}"#);
        assert!(r.contains("\"formatted\":3"), "{r}");
        assert_spill_bold(&s, true, "bold");
        s.dispatch("undo");
        assert_spill_bold(&s, false, "undo");
        s.dispatch("redo");
        assert_spill_bold(&s, true, "redo");
    }

    // -- interactive `fmt`/`decimals`/`autosum` (toolbar verbs) ------------

    #[test]
    fn dispatch_fmt_bold_toggle_flips_the_whole_selection_as_one_undo_group() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t0\t0\t1\t1"); // A1:B2, active cell A1
        s.dispatch("fmt\tbold\ttoggle");
        for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            let style = s.pkg.workbook.sheets[0]
                .cell(r, c)
                .map(|cl| cl.style)
                .unwrap_or(0);
            assert!(
                s.pkg.workbook.styles.xf(style).bold,
                "({r},{c}) must be bold after toggle"
            );
        }
        // Values are untouched by the format-only edit.
        assert_eq!(s.cell_seed(1, 0), "Apple");
        assert_eq!(s.cell_seed(1, 1), "1.25");

        s.dispatch("undo"); // ONE undo restores all four cells
        for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            let style = s.pkg.workbook.sheets[0]
                .cell(r, c)
                .map(|cl| cl.style)
                .unwrap_or(0);
            assert!(
                !s.pkg.workbook.styles.xf(style).bold,
                "one undo must clear bold at ({r},{c})"
            );
        }
    }

    #[test]
    fn dispatch_fmt_bold_toggle_reads_the_active_cells_state_not_the_selections() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        // Pre-bold the active cell only; the rest of the selection starts
        // unbold. Excel toggles OFF because the active cell is bold, and
        // applies that target state to the WHOLE selection.
        s.dispatch("select\t0\t0");
        s.dispatch("fmt\tbold\ttoggle"); // A1 -> bold
        let a1_style = s.pkg.workbook.sheets[0]
            .cell(0, 0)
            .map(|cl| cl.style)
            .unwrap_or(0);
        assert!(s.pkg.workbook.styles.xf(a1_style).bold);

        s.dispatch("select\t0\t0\t1\t1"); // A1:B2, active cell A1 (bold)
        s.dispatch("fmt\tbold\ttoggle"); // must turn OFF for the whole range
        for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            let style = s.pkg.workbook.sheets[0]
                .cell(r, c)
                .map(|cl| cl.style)
                .unwrap_or(0);
            assert!(
                !s.pkg.workbook.styles.xf(style).bold,
                "({r},{c}) must be un-bold: toggle followed the active cell"
            );
        }
    }

    #[test]
    fn dispatch_fmt_italic_align_font_color_fill_color_numfmt_apply_and_undo() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t1\t1"); // B2 (1.25)
        s.dispatch("fmt\titalic\ttoggle");
        s.dispatch("fmt\talign\tcenter");
        s.dispatch("fmt\tfontColor\t#FF0000");
        s.dispatch("fmt\tfillColor\t#00FF00");
        s.dispatch("fmt\tnumFmt\t0.00%");
        let style = s.pkg.workbook.sheets[0]
            .cell(1, 1)
            .map(|cl| cl.style)
            .unwrap_or(0);
        let xf = s.pkg.workbook.styles.xf(style);
        assert!(xf.italic);
        assert_eq!(xf.align, gridcore::sheet::Align::Center);
        assert_eq!(xf.color, Some((255, 0, 0)));
        assert_eq!(xf.fill, Some((0, 255, 0)));
        assert_eq!(xf.code.as_deref(), Some("0.00%"));

        for _ in 0..5 {
            s.dispatch("undo");
        }
        let style = s.pkg.workbook.sheets[0]
            .cell(1, 1)
            .map(|cl| cl.style)
            .unwrap_or(0);
        let xf = s.pkg.workbook.styles.xf(style);
        assert!(!xf.italic);
        assert_eq!(xf.align, gridcore::sheet::Align::General);
        assert_eq!(xf.color, None);
        assert_eq!(xf.fill, None);
        assert_ne!(xf.code.as_deref(), Some("0.00%"));
    }

    #[test]
    fn dispatch_fmt_unknown_key_is_a_no_op() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let undo_before = s.undo.len();
        s.dispatch("fmt\twrap\ttrue");
        assert_eq!(
            s.undo.len(),
            undo_before,
            "an unrecognized fmt key must not mutate"
        );
    }

    #[test]
    fn dispatch_decimals_increases_and_decreases_the_selections_numfmt() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t1\t1"); // B2 (1.25), unstyled -> General
        s.dispatch("decimals\t1");
        let style = s.pkg.workbook.sheets[0]
            .cell(1, 1)
            .map(|cl| cl.style)
            .unwrap_or(0);
        assert_eq!(s.pkg.workbook.styles.xf(style).code.as_deref(), Some("0.0"));
        s.dispatch("decimals\t1");
        let style = s.pkg.workbook.sheets[0]
            .cell(1, 1)
            .map(|cl| cl.style)
            .unwrap_or(0);
        assert_eq!(
            s.pkg.workbook.styles.xf(style).code.as_deref(),
            Some("0.00")
        );
        s.dispatch("decimals\t-1");
        let style = s.pkg.workbook.sheets[0]
            .cell(1, 1)
            .map(|cl| cl.style)
            .unwrap_or(0);
        assert_eq!(s.pkg.workbook.styles.xf(style).code.as_deref(), Some("0.0"));
    }

    #[test]
    fn dispatch_autosum_guesses_the_column_above_run() {
        let mut s = Session::open(&new_xlsx_bytes()).expect("open");
        s.dispatch("set\t0\t0\t10"); // A1
        s.dispatch("set\t1\t0\t20"); // A2
        s.dispatch("set\t2\t0\t30"); // A3
        s.dispatch("select\t3\t0"); // A4, active cell
        s.dispatch("autosum");
        assert_eq!(s.cell_seed(3, 0), "=SUM(A1:A3)");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"=SUM(A1:A3)\""), "{v}");
        s.dispatch("select\t3\t0");
        let v = s.view_json(None);
        assert!(v.contains("60"), "SUM must evaluate: {v}");
    }

    #[test]
    fn dispatch_autosum_falls_back_to_the_row_to_the_left_when_nothing_is_above() {
        let mut s = Session::open(&new_xlsx_bytes()).expect("open");
        s.dispatch("set\t0\t0\t5"); // A1
        s.dispatch("set\t0\t1\t15"); // B1
        s.dispatch("select\t0\t2"); // C1, active cell, nothing above (row 0)
        s.dispatch("autosum");
        assert_eq!(s.cell_seed(0, 2), "=SUM(A1:B1)");
    }

    #[test]
    fn dispatch_autosum_is_a_no_op_with_an_error_when_nothing_to_sum() {
        let mut s = Session::open(&new_xlsx_bytes()).expect("open");
        s.dispatch("select\t5\t5"); // isolated, blank neighbors
        let undo_before = s.undo.len();
        s.dispatch("autosum");
        assert_eq!(
            s.undo.len(),
            undo_before,
            "no-op must not push an undo group"
        );
        assert_eq!(s.cell_seed(5, 5), "");
        let v = s.view_json(None);
        assert!(
            v.contains("AutoSum: no numbers to sum"),
            "must surface an error: {v}"
        );
    }

    #[test]
    fn dispatch_autosum_no_op_does_not_bump_the_edits_counter_but_a_real_one_does() {
        // The webview's `userCmd` (grid.js) posts `{type:'edit'}` to the host
        // iff `edits` increased across a dispatch call — this is the fix for
        // the bug where AutoSum-with-nothing-to-sum used to desync VS Code's
        // undo stack via a verb-name regex that couldn't tell "dispatched"
        // from "actually mutated".
        let mut s = Session::open(&new_xlsx_bytes()).expect("open");
        s.dispatch("select\t5\t5"); // isolated, blank neighbors
        let edits_before = s.edits;
        s.dispatch("autosum");
        assert_eq!(
            s.edits, edits_before,
            "a no-op AutoSum must not bump the edits counter"
        );
        // A real edit right after DOES bump it by exactly one.
        s.dispatch("set\t5\t5\t42");
        assert_eq!(
            s.edits,
            edits_before + 1,
            "a real edit must bump the edits counter by exactly one"
        );
    }

    #[test]
    fn do_undo_and_do_redo_never_bump_the_edits_counter() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t1\t1\t10");
        let after_edit = s.edits;
        s.dispatch("undo");
        assert_eq!(s.edits, after_edit, "undo must not bump edits");
        s.dispatch("redo");
        assert_eq!(s.edits, after_edit, "redo must not bump edits");
    }

    #[test]
    fn dispatch_fmt_rejects_a_selection_over_the_cap_and_touches_nothing() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        // A single-column selection one row past the cap: CELL_FORMAT_CAP+1
        // cells, same boundary gridcore::format's own suite pins.
        s.dispatch(&format!(
            "select\t0\t0\t{}\t0",
            gridcore::format::CELL_FORMAT_CAP
        ));
        let undo_before = s.undo.len();
        let edits_before = s.edits;
        s.dispatch("fmt\tbold\ttoggle");
        assert_eq!(
            s.undo.len(),
            undo_before,
            "an oversized selection must not apply"
        );
        assert_eq!(
            s.edits, edits_before,
            "a rejected format must not bump edits"
        );
        let v = s.view_json(None);
        assert!(
            v.contains(&format!(
                "cell.format: range too large (limit {} cells)",
                gridcore::format::CELL_FORMAT_CAP
            )),
            "{v}"
        );
    }

    #[test]
    fn view_json_cur_carries_the_active_cells_bold_italic_and_align() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t1\t1"); // B2
        s.dispatch("fmt\tbold\ttoggle");
        s.dispatch("fmt\titalic\ttoggle");
        s.dispatch("fmt\talign\tcenter");
        let v = s.view_json(None);
        assert!(v.contains("\"bold\":true"), "{v}");
        assert!(v.contains("\"italic\":true"), "{v}");
        assert!(v.contains("\"align\":\"c\""), "{v}");
    }

    #[test]
    fn view_json_cur_align_defaults_to_g_for_general() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let v = s.view_json(None);
        assert!(v.contains("\"bold\":false"), "{v}");
        assert!(v.contains("\"italic\":false"), "{v}");
        assert!(v.contains("\"align\":\"g\""), "{v}");
    }

    fn new_xlsx_bytes() -> Vec<u8> {
        save_xlsx(&new_xlsx())
    }

    #[test]
    fn save_round_trips_losslessly_and_clears_dirty() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t1\t1\t10");
        let out = s.save();
        let v = s.view_json(None);
        assert!(v.contains("\"dirty\":false"), "{v}");
        let mut s2 = Session::open(&out).expect("reopen");
        let v2 = s2.view_json(None);
        assert!(v2.contains("12.5"), "edit persisted through save: {v2}");
    }

    #[test]
    fn new_workbook_bytes_open() {
        let bytes = new_workbook();
        let mut s = Session::open(&bytes).expect("open fresh workbook");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");
    }

    #[test]
    fn paste_strips_crlf_and_trailing_newline() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        // Pre-existing value at row index 7, col 0 — must survive the paste.
        // A naive `split('\n')` over a trailing-newline TSV yields a phantom
        // empty last line one row past the pasted data, which would clear it.
        s.dispatch("set\t7\t0\tsentinel");
        // B7's field is text ("x"), not numeric: parse_input trims whitespace
        // (which \r counts as) before parsing a number, so a numeric field
        // would still read back clean even without the fix and mask the bug.
        // A text field takes the untrimmed literal, so it's the only field
        // that actually proves the per-line \r is stripped.
        s.dispatch("paste\t5\t0\ta\t1\r\nb\tx\r\n");
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"a\""), "A6 must be clean 'a': {v}");
        s.dispatch("select\t6\t1");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"x\""),
            "B7 must be clean 'x', no \\r: {v}"
        );
        s.dispatch("select\t7\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"sentinel\""),
            "phantom empty last line must not clear row 7: {v}"
        );
    }

    #[test]
    fn paste_demotes_invalid_formula_to_text() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("paste\t6\t0\t=BOGUS((");
        let v = s.view_json(None);
        // An unparseable formula must not be stored as a formula (which
        // would render blank/frozen since it never evaluates) — it must be
        // demoted to literal text so the cell actually shows something.
        assert!(
            v.contains("\"r\":6,\"c\":0,\"t\":\"=BOGUS((\""),
            "invalid formula must demote to visible literal text: {v}"
        );
        s.dispatch("select\t6\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=BOGUS((\""),
            "src should read back the literal text: {v}"
        );
    }

    #[test]
    fn sheet_add_rename_switch() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("sheet\tadd\tData");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Data\"]"), "{v}");
        assert!(
            v.contains("\"active\":1"),
            "add switches to the new sheet: {v}"
        );
        s.dispatch("sheet\trename\t1\tFacts");
        let v = s.view_json(None);
        assert!(v.contains("Facts"), "{v}");
        s.dispatch("sheet\tswitch\t0");
        let v = s.view_json(None);
        assert!(v.contains("\"active\":0") && v.contains("Apple"), "{v}");
    }

    #[test]
    fn sheet_add_is_undoable_in_lockstep() {
        // The host registers one undo step per mutating command; if sheet
        // add clears the engine's undo/redo stacks instead of pushing onto
        // them, the two stacks desync and VS Code's undo can walk the
        // document back past a point the engine can actually reach.
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t5\t0\thello");
        s.dispatch("sheet\tadd\tData");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Data\"]"), "{v}");

        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\"]"),
            "first undo removes the added sheet: {v}"
        );
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"hello\""),
            "the cell edit is a separate, still-undone step: {v}"
        );

        s.dispatch("undo");
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"\""),
            "second undo reverts the cell edit: {v}"
        );

        s.dispatch("redo");
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"hello\""),
            "first redo reapplies the cell edit: {v}"
        );

        s.dispatch("redo");
        let v = s.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\",\"Data\"]"),
            "second redo re-adds the sheet: {v}"
        );
    }

    #[test]
    fn sheet_add_undo_survives_save() {
        // A `Structural` before/after snapshot only rolls back
        // `workbook.sheets` — it doesn't remove the minted worksheet part or
        // its <sheet> entry in xl/workbook.xml. That leftover entry outlives
        // the undo, so on save the model has 1 sheet but workbook.xml still
        // has 2 `<sheet>` elements; `patch_sheet_names` bails on the count
        // mismatch (see gridcore::xlsx::patch_sheet_names) and the saved
        // file still lists the "undone" sheet, which resurrects (empty) on
        // reopen. `SheetAdd`'s inverse (`remove_sheet`) must remove the part
        // and its workbook.xml entry for real, not just the model entry.
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("sheet\tadd\tData");
        s.dispatch("undo");
        let out = s.save();
        let mut reopened = Session::open(&out).expect("reopen");
        let v = reopened.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\"]"),
            "the undone sheet must not resurrect on reopen: {v}"
        );
    }

    // -----------------------------------------------------------------------
    // Task 3: `grid_ctl` agent control surface
    // -----------------------------------------------------------------------

    #[test]
    fn ctl_sheet_read_and_cell_get() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"sheet.read","args":{}}"#);
        assert!(
            out.contains("\"ok\":true") && out.contains("Apple"),
            "{out}"
        );
        let out = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B4"}}"#);
        assert!(out.contains("SUM(B1:B3)"), "{out}");
    }

    #[test]
    fn set_refuses_an_entry_over_the_cell_limit_658() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t0\t0\told");
        s.err = None;
        s.dispatch(&format!("set\t0\t0\t{}", "y".repeat(32_768)));
        let err = s.err.clone().expect("an error");
        assert!(err.contains("32767"), "{err}");
        assert_eq!(s.cell_seed(0, 0), "old");
        let r = s.ctl(&format!(
            r#"{{"verb":"cell.set","args":{{"ref":"A1","text":"{}"}}}}"#,
            "y".repeat(32_768)
        ));
        assert!(r.contains("32767"), "{r}");
    }

    #[test]
    fn a_text_beginning_with_an_apostrophe_survives_copy_and_paste() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let a = s.active;
        // A loaded shared string 'abc with no quote prefix (General).
        s.pkg.workbook.sheets[a].set_cell(40, 0, Cell::text("'abc"));
        assert_eq!(s.cell_seed(40, 0), "''abc");
        s.cur = (40, 0);
        s.anchor = None;
        let tsv = s.dispatch("copy").expect("copy returns the TSV");
        s.dispatch(&format!("paste\t41\t0\t{tsv}"));
        let pasted = s.pkg.workbook.sheets[a].cell(41, 0).unwrap().clone();
        assert_eq!(pasted.value, CellValue::Text("'abc".into()));
        // A Text-formatted cell holding 'abc, copied into another Text cell.
        let text = s.pkg.workbook.styles.intern(gridcore::sheet::Xf {
            numfmt: gridcore::sheet::NumFmt::Text,
            code: Some("@".into()),
            ..Default::default()
        });
        for r in [42, 43] {
            s.pkg.workbook.sheets[a].set_cell(
                r,
                0,
                Cell {
                    style: text,
                    ..Cell::default()
                },
            );
        }
        s.dispatch("set\t42\t0\t'abc");
        assert_eq!(
            s.cell_seed(42, 0),
            "'abc",
            "a Text cell keeps the entry as typed"
        );
        assert_eq!(s.cell_seed(42, 0), "'abc");
        s.cur = (42, 0);
        let tsv = s.dispatch("copy").expect("copy returns the TSV");
        s.dispatch(&format!("paste\t43\t0\t{tsv}"));
        let pasted = s.pkg.workbook.sheets[a].cell(43, 0).unwrap().clone();
        assert_eq!(pasted.value, CellValue::Text("'abc".into()));
        assert!(!s.pkg.workbook.styles.xf(pasted.style).quote_prefix);
    }

    #[test]
    fn a_text_cell_takes_a_broken_formula_as_text() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let text = s.pkg.workbook.styles.intern(gridcore::sheet::Xf {
            numfmt: gridcore::sheet::NumFmt::Text,
            code: Some("@".into()),
            ..Default::default()
        });
        let a = s.active;
        s.pkg.workbook.sheets[a].set_cell(
            30,
            0,
            Cell {
                style: text,
                ..Cell::default()
            },
        );
        s.err = None;
        s.dispatch("set\t30\t0\t=SUM(");
        assert_eq!(s.err, None);
        assert_eq!(s.cell_seed(30, 0), "=SUM(");
        s.dispatch("set\t31\t0\t=SUM(");
        assert!(s.err.is_some(), "a General cell still refuses it");
    }

    #[test]
    fn set_reads_entries_the_way_excel_does() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t20\t0\t'007");
        assert_eq!(s.cell_seed(20, 0), "'007");
        // Copy then paste gives back quote-prefixed text, not the number 7.
        s.cur = (20, 0);
        s.anchor = None;
        let tsv = s.dispatch("copy").expect("copy returns the TSV");
        assert_eq!(tsv.trim_end(), "'007");
        s.dispatch(&format!("paste\t22\t0\t{tsv}"));
        let pasted = s.pkg.workbook.sheets[s.active].cell(22, 0).unwrap().clone();
        assert_eq!(pasted.value, CellValue::Text("007".into()));
        assert!(s.pkg.workbook.styles.xf(pasted.style).quote_prefix);
        let cell = s.pkg.workbook.sheets[s.active].cell(20, 0).unwrap().clone();
        assert_eq!(cell.value, CellValue::Text("007".into()));
        s.dispatch("set\t21\t0\t$1,234.56");
        let cell = s.pkg.workbook.sheets[s.active].cell(21, 0).unwrap().clone();
        assert_eq!(cell.value, CellValue::Number(1234.56));
    }

    #[test]
    fn ctl_cell_set_recalculates_and_undoes() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"cell.set","args":{"ref":"B2","text":"10"}}"#);
        let v = s.view_json(None);
        assert!(v.contains("12.5"), "recalc: {v}");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(v.contains("3.75"), "one undo restores: {v}");
    }

    #[test]
    fn ctl_invalid_formula_and_bad_ref_error() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        assert!(
            s.ctl(r#"{"verb":"cell.set","args":{"ref":"B2","text":"=SUM("}}"#)
                .contains("\"ok\":false")
        );
        assert!(
            s.ctl(r#"{"verb":"cell.get","args":{"ref":"NOPE99X"}}"#)
                .contains("\"ok\":false")
        );
    }

    #[test]
    fn ctl_sheet_list_and_wb_info() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"sheet.list","args":{}}"#);
        assert!(
            out.contains("\"active\":0") && out.contains("\"name\":\"Sheet1\""),
            "{out}"
        );
        let out = s.ctl(r#"{"verb":"wb.info","args":{}}"#);
        assert!(
            out.contains("\"sheets\":1")
                && out.contains("\"active\":0")
                && out.contains("\"modified\":false")
                && out.contains("\"ok\":true"),
            "{out}"
        );
        s.ctl(r#"{"verb":"cell.set","args":{"ref":"A1","text":"x"}}"#);
        let out = s.ctl(r#"{"verb":"wb.info","args":{}}"#);
        assert!(
            out.contains("\"modified\":true"),
            "edits must flip modified: {out}"
        );
    }

    #[test]
    fn ctl_range_clear_is_undoable() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"range.clear","args":{"range":"A1:B1"}}"#);
        assert!(
            out.contains("\"cleared\":2") && out.contains("\"ok\":true"),
            "{out}"
        );
        let v = s.view_json(None);
        assert!(!v.contains("Item") && !v.contains("Price"), "{v}");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(v.contains("Item") && v.contains("Price"), "{v}");
    }

    #[test]
    fn ctl_find_scans_values_and_formulas() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"find","args":{"query":"apple"}}"#);
        assert!(
            out.contains("\"count\":1") && out.contains("\"ref\":\"A2\""),
            "{out}"
        );
        let out = s.ctl(r#"{"verb":"find","args":{"query":"sum"}}"#);
        assert!(
            out.contains("\"count\":1") && out.contains("\"ref\":\"B4\""),
            "{out}"
        );
    }

    #[test]
    fn ctl_sheet_arg_accepts_index_and_name() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"sheet.read","args":{"sheet":"Sheet1"}}"#);
        assert!(
            out.contains("\"ok\":true") && out.contains("Apple"),
            "{out}"
        );
        let out = s.ctl(r#"{"verb":"sheet.read","args":{"sheet":9}}"#);
        assert!(out.contains("\"ok\":false"), "{out}");
    }

    #[test]
    fn ctl_unknown_verb_and_bad_json() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"wb.frobnicate","args":{}}"#);
        assert!(out.contains("unknown verb 'wb.frobnicate'"), "{out}");
        let out = s.ctl("not json");
        assert!(out.contains("\"ok\":false"), "{out}");
    }

    #[test]
    fn ctl_wb_recalc() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"wb.recalc","args":{}}"#);
        assert!(
            out.contains("\"recalculated\":true") && out.contains("\"ok\":true"),
            "{out}"
        );
    }

    #[test]
    fn sheet_add_undo_then_rename_persists_through_save() {
        // Same root cause as `sheet_add_undo_survives_save`, but the fallout
        // is worse: with the model/workbook.xml <sheet>-count mismatch left
        // behind by an unremoved part, `patch_sheet_names` bails on *every*
        // name in the workbook, not just the orphaned one — so a rename
        // saved during the mismatched session was silently dropped too.
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("sheet\tadd\tData");
        s.dispatch("undo");
        s.dispatch("sheet\trename\t0\tRenamed");
        let out = s.save();
        let mut reopened = Session::open(&out).expect("reopen");
        let v = reopened.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Renamed\"]"),
            "the rename must persist through save: {v}"
        );
    }

    // -----------------------------------------------------------------------
    // Task 6: wave-1 `grid_ctl` verbs
    // -----------------------------------------------------------------------

    /// Pull a JSON number out of a ctl reply body by key (avoids brittle
    /// float-string matching for `sheet.stats`/`formula.eval`).
    fn num(body: &str, key: &str) -> f64 {
        match json::Json::parse(body).unwrap().get(key) {
            Some(json::Json::Num(n)) => *n,
            other => panic!("expected a number at '{key}', got {other:?} in {body}"),
        }
    }

    /// A second, blank sheet added to `sample_xlsx()`, with `active` restored
    /// to Sheet1 and the undo/redo stacks cleared so mutating-verb tests
    /// start from a clean, known baseline.
    fn two_sheet_session() -> Session {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("sheet\tadd\tSheet2");
        s.active = 0;
        s.cur = (0, 0);
        s.anchor = None;
        s.undo.clear();
        s.redo.clear();
        s
    }

    fn pivot_fixture(s: &mut Session) {
        s.dispatch("set\t0\t0\tname");
        s.dispatch("set\t0\t1\tamount");
        s.dispatch("set\t1\t0\tAlice");
        s.dispatch("set\t1\t1\t10");
        s.dispatch("set\t2\t0\tBob");
        s.dispatch("set\t2\t1\t20");
        s.dispatch("set\t3\t0\tAlice");
        s.dispatch("set\t3\t1\t20");
    }

    // -- read verbs -----------------------------------------------------

    #[test]
    fn ctl_comment_list_empty_on_plain_fixture() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        assert!(
            out.contains("\"comments\":[]") && out.contains("\"ok\":true"),
            "{out}"
        );
    }

    #[test]
    fn ctl_comment_list_flattens_in_reply_order() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.pkg.set_comment(0, 1, 2, "Reviewer", "Check this value");
        s.pkg
            .add_threaded_comment(0, 3, 0, "Ana", "A note", "2024-01-02T03:04:05Z");
        let out = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        assert!(
            out.contains(
                "{\"sheet\":0,\"ref\":\"C2\",\"author\":\"Reviewer\",\"text\":\"Check this value\"}"
            ),
            "{out}"
        );
        assert!(out.contains("\"ref\":\"A4\",\"author\":\"Ana\""), "{out}");
    }

    #[test]
    fn ctl_wb_export_csv_returns_display_formatted_text() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"wb.export-csv","args":{}}"#);
        assert!(out.contains("\"sheet\":0"), "{out}");
        // Excel's CSV text: CR LF records, as xlsxy's verb returns.
        assert!(out.contains(r"Item,Price\r\nApple,1.25"), "{out}");
        assert!(out.contains("3.75"), "{out}");
    }

    #[test]
    fn ctl_sheet_pivot_sums_by_group_and_is_read_only() {
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        let before = s.view_json(None);
        let out = s.ctl(
            r#"{"verb":"sheet.pivot","args":{"range":"A1:B4","rows":["name"],"values":[{"col":"amount","agg":"sum"}]}}"#,
        );
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(out.contains(r#"["name","Sum of amount"]"#), "{out}");
        assert!(out.contains(r#"["Alice","30"]"#), "{out}");
        assert!(out.contains(r#"["Bob","20"]"#), "{out}");
        let after = s.view_json(None);
        assert_eq!(before, after, "sheet.pivot must not mutate the session");
    }

    #[test]
    fn ctl_sheet_pivot_unknown_header_names_the_column() {
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        let out =
            s.ctl(r#"{"verb":"sheet.pivot","args":{"range":"A1:B4","rows":["nope"],"values":[]}}"#);
        assert!(
            out.contains("\"ok\":false") && out.contains("nope"),
            "{out}"
        );
    }

    #[test]
    fn ctl_formula_eval_returns_value_and_text_without_mutating() {
        let mut s = Session::open(&new_workbook()).expect("open");
        s.dispatch("set\t0\t0\t10");
        let before = s.view_json(None);
        let out = s.ctl(r#"{"verb":"formula.eval","args":{"formula":"=A1+1","ref":"B5"}}"#);
        assert_eq!(num(&out, "value"), 11.0, "{out}");
        assert!(out.contains("\"text\":\"11\""), "{out}");
        let after = s.view_json(None);
        assert_eq!(before, after, "formula.eval must not mutate anything");
    }

    #[test]
    fn ctl_formula_eval_defaults_ref_and_reports_errors() {
        let mut s = Session::open(&new_workbook()).expect("open");
        let out = s.ctl(r#"{"verb":"formula.eval","args":{"formula":"=1/0"}}"#);
        assert!(out.contains("\"text\":\"#DIV/0!\""), "{out}");
    }

    #[test]
    fn ctl_sheet_stats_returns_all_six_keys() {
        let mut s = Session::open(&new_workbook()).expect("open");
        s.dispatch("set\t0\t0\t10");
        s.dispatch("set\t1\t0\t20");
        s.dispatch("set\t2\t0\t-5");
        let out = s.ctl(r#"{"verb":"sheet.stats","args":{"range":"A1:A3"}}"#);
        assert_eq!(num(&out, "sum"), 25.0, "{out}");
        assert_eq!(num(&out, "count"), 3.0, "{out}");
        assert_eq!(num(&out, "countNums"), 3.0, "{out}");
        assert!((num(&out, "average") - 25.0 / 3.0).abs() < 1e-9, "{out}");
        assert_eq!(num(&out, "min"), -5.0, "{out}");
        assert_eq!(num(&out, "max"), 20.0, "{out}");
    }

    #[test]
    fn ctl_chart_list_empty_on_plain_fixture() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"chart.list","args":{}}"#);
        assert!(out.contains("\"charts\":[]"), "{out}");
    }

    #[test]
    fn ctl_chart_list_reports_kind_title_categories_and_series() {
        use gridcore::sheet::{ChartData, ChartSeries, Drawing, DrawingKind as DK};
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.pkg.workbook.sheets[0].drawings.push(Drawing {
            anchor_ix: 0,
            from: (0, 0),
            to: (10, 5),
            kind: DK::Chart(ChartData {
                title: "Sales".into(),
                kind: "bar".into(),
                categories: vec!["North".into(), "South".into()],
                series: vec![ChartSeries {
                    name: "Q1".into(),
                    values: vec![10.0, 20.0],
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        let out = s.ctl(r#"{"verb":"chart.list","args":{}}"#);
        assert!(
            out.contains("\"kind\":\"bar\"") && out.contains("\"title\":\"Sales\""),
            "{out}"
        );
        assert!(
            out.contains("\"categories\":[\"North\",\"South\"]"),
            "{out}"
        );
        assert!(
            out.contains("\"name\":\"Q1\"") && out.contains("\"values\":[10,20]"),
            "{out}"
        );
    }

    #[test]
    fn ctl_pivot_list_empty_on_plain_fixture() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"pivot.list","args":{}}"#);
        assert!(out.contains("\"pivots\":[]"), "{out}");
    }

    #[test]
    fn ctl_pivot_list_summarizes_rows_cols_and_values() {
        use gridcore::pivot::{DataField, Pivot, PivotSource};
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.pkg.workbook.pivots.push(Pivot {
            name: "P".into(),
            sheet: 0,
            location: (0, 3, 0, 3),
            source: PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 3, 1),
            },
            fields: vec!["Region".into(), "Sales".into()],
            row_fields: vec![0],
            col_fields: vec![],
            data_fields: vec![DataField {
                name: "Sum of Sales".into(),
                field: 1,
                agg: gridcore::frame::Agg::Sum,
            }],
            field_items: Vec::new(),
            hidden: Vec::new(),
            page: Vec::new(),
            items_order: Vec::new(),
            calc_formulas: Vec::new(),
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            data_on_rows: false,
            unsupported: false,
            edited: false,
            part: String::new(),
            cache_part: String::new(),
        });
        let out = s.ctl(r#"{"verb":"pivot.list","args":{}}"#);
        assert!(out.contains("\"sheet\":0"), "{out}");
        assert!(out.contains("\"rows\":[\"Region\"]"), "{out}");
        assert!(out.contains("\"cols\":[]"), "{out}");
        assert!(out.contains("\"values\":[\"Sum of Sales\"]"), "{out}");
    }

    // -- pivot.create: Wave-3 Task 4 (byte-parity mirror of xlsxy
    // control.rs's `pivot_create`, plus the wasm-only inverse/undoSteps
    // envelope) ----------------------------------------------------------

    /// `pivot.create` args over `pivot_fixture`'s A1:B4 name/amount data,
    /// same shape as xlsxy control.rs's own `pivot_create_args` test helper.
    fn pivot_create_args(name: Option<&str>) -> String {
        match name {
            Some(n) => format!(
                r#"{{"verb":"pivot.create","args":{{"range":"A1:B4","rows":["name"],"values":[{{"col":"amount","agg":"sum"}}],"name":"{n}"}}}}"#
            ),
            None => r#"{"verb":"pivot.create","args":{"range":"A1:B4","rows":["name"],"values":[{"col":"amount","agg":"sum"}]}}"#.to_string(),
        }
    }

    #[test]
    fn ctl_pivot_create_returns_sheet_and_name_lists_and_computes() {
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        let out = s.ctl(&pivot_create_args(None));
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(out.contains("\"name\":\"Pivot1\""), "{out}");
        assert!(
            out.contains("\"sheet\":1"),
            "the pivot must land on a NEW sheet: {out}"
        );

        let lst = s.ctl(r#"{"verb":"pivot.list","args":{}}"#);
        assert!(lst.contains("\"sheet\":1"), "{lst}");
        assert!(lst.contains("\"rows\":[\"name\"]"), "{lst}");
        assert!(lst.contains("\"values\":[\"Sum of amount\"]"), "{lst}");

        // The output sheet already holds computed values (0-indexed row 2 =
        // header, row 3 = first row group — "Alice" sorts first, total
        // 10+20=30 — i.e. A1 ref "B4").
        let g = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B4","sheet":1}}"#);
        assert!(g.contains("\"value\":30"), "{g}");
    }

    #[test]
    fn ctl_pivot_create_default_names_are_unique_pivotn() {
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        let r1 = s.ctl(&pivot_create_args(None));
        assert!(r1.contains("\"name\":\"Pivot1\""), "{r1}");
        let r2 = s.ctl(&pivot_create_args(None));
        assert!(r2.contains("\"name\":\"Pivot2\""), "{r2}");
    }

    #[test]
    fn ctl_pivot_create_explicit_name_is_used_and_duplicate_errors() {
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        let r = s.ctl(&pivot_create_args(Some("ByName")));
        assert!(r.contains("\"name\":\"ByName\""), "{r}");
        let err = s.ctl(&pivot_create_args(Some("ByName")));
        assert!(
            err.contains("\"ok\":false") && err.contains("ByName"),
            "{err}"
        );
        // Colliding with a plain (non-pivot) sheet name errors the same way.
        let err2 = s.ctl(&pivot_create_args(Some("Sheet1")));
        assert!(
            err2.contains("\"ok\":false") && err2.contains("Sheet1"),
            "{err2}"
        );
    }

    #[test]
    fn ctl_pivot_create_unknown_header_names_the_column_like_sheet_pivot() {
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        let out = s.ctl(
            r#"{"verb":"pivot.create","args":{"range":"A1:B4","rows":["nope"],"values":[{"col":"amount","agg":"sum"}]}}"#,
        );
        assert!(
            out.contains("\"ok\":false") && out.contains("nope"),
            "{out}"
        );
        assert!(out.contains("pivot.create:"), "{out}");
    }

    #[test]
    fn ctl_pivot_create_needs_at_least_one_value_field() {
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        let out = s
            .ctl(r#"{"verb":"pivot.create","args":{"range":"A1:B4","rows":["name"],"values":[]}}"#);
        assert!(
            out.contains("\"ok\":false") && out.contains("value field"),
            "{out}"
        );
    }

    #[test]
    fn ctl_pivot_create_clears_history_and_its_declared_inverse_is_both_or_neither() {
        // Empirical bucket (Task 3's report §3, Task 4's brief): same as
        // `sheet.import-csv` — a new sheet + pivot-part registration isn't a
        // cell-level edit the wasm undo stack can invert, so history is
        // cleared rather than pushing a true undo entry.
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        s.dispatch("set\t5\t0\thello"); // pushes one undo entry, must be wiped
        let out = s.ctl(&pivot_create_args(None));
        assert!(out.contains("\"undoSteps\":0"), "{out}");
        assert!(
            out.contains("\"inverse\":{\"verb\":\"sheet.remove\",\"args\":{\"sheet\":1}}"),
            "{out}"
        );

        // History was cleared, not merely left un-added-to.
        s.dispatch("undo");
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"hello\""),
            "pivot.create must clear existing undo history: {v}"
        );

        // Applying the declared inverse removes BOTH the sheet AND the
        // pivot registration — a both-or-neither check: neither survives
        // alone (a sheet-only inverse leaving a dangling pivot.list entry,
        // or a pivot-only removal leaving an orphaned sheet, is a defect).
        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":1}}"#);
        let v2 = s.view_json(None);
        assert!(v2.contains("\"sheets\":[\"Sheet1\"]"), "{v2}");
        let lst = s.ctl(r#"{"verb":"pivot.list","args":{}}"#);
        assert!(lst.contains("\"pivots\":[]"), "{lst}");
    }

    #[test]
    fn ctl_wb_recalc_refreshes_pivot_output_after_a_source_edit() {
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        s.ctl(&pivot_create_args(None));
        // Bob's amount 20 -> 200 on the SOURCE sheet (Bob is the second
        // source row, at A1 ref "B3"); the pivot output's Bob group lands
        // at "B5" (0-indexed row 4: row2=header, row3=Alice, row4=Bob).
        s.ctl(r#"{"verb":"cell.set","args":{"ref":"B3","text":"200","sheet":0}}"#);
        s.ctl(r#"{"verb":"wb.recalc","args":{}}"#);
        let g = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B5","sheet":1}}"#);
        assert!(g.contains("\"value\":200"), "{g}");
    }

    #[test]
    fn ctl_sheet_remove_restore_round_trips_a_pivot_registration() {
        // Wave-3 Task 4 review finding (mandatory fix): `remove_sheet`'s
        // pivot cascade (Task 3, needed so `pivot.create`'s inverse is
        // "both or neither") would otherwise silently outrun this stash —
        // a sheet.remove + sheet.restore-removed round trip on a
        // pivot-hosting sheet brought the sheet back WITHOUT its pivot,
        // even though every cell value (including the pivot's own
        // previously-computed output) was restored correctly.
        let mut s = Session::open(&new_workbook()).expect("open");
        pivot_fixture(&mut s);
        let out = s.ctl(&pivot_create_args(None));
        assert!(
            out.contains("\"sheet\":1") && out.contains("\"name\":\"Pivot1\""),
            "{out}"
        );

        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":1}}"#);
        let lst = s.ctl(r#"{"verb":"pivot.list","args":{}}"#);
        assert!(
            lst.contains("\"pivots\":[]"),
            "the cascade must drop the pivot with its sheet: {lst}"
        );

        let restore = s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        assert!(
            restore.contains("\"sheet\":1") && restore.contains("\"name\":\"Pivot1\""),
            "{restore}"
        );

        // pivot.list shows it again...
        let lst2 = s.ctl(r#"{"verb":"pivot.list","args":{}}"#);
        assert!(lst2.contains("\"sheet\":1"), "{lst2}");
        assert!(lst2.contains("\"rows\":[\"name\"]"), "{lst2}");
        assert!(lst2.contains("\"values\":[\"Sum of amount\"]"), "{lst2}");

        // ...AND wb.recalc genuinely refreshes it — not just a stale
        // metadata entry. Same B3 (source) -> B5 (output) edit as
        // `ctl_wb_recalc_refreshes_pivot_output_after_a_source_edit`.
        s.ctl(r#"{"verb":"cell.set","args":{"ref":"B3","text":"200","sheet":0}}"#);
        s.ctl(r#"{"verb":"wb.recalc","args":{}}"#);
        let g = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B5","sheet":1}}"#);
        assert!(g.contains("\"value\":200"), "{g}");
    }

    // -- comment.add / comment.remove: NOT on the undo stack ------------

    #[test]
    fn ctl_comment_add_returns_sheet_ref_and_a_working_inverse() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(
            r#"{"verb":"comment.add","args":{"ref":"B2","text":"Check this","author":"Ana"}}"#,
        );
        assert!(
            out.contains("\"sheet\":0") && out.contains("\"ref\":\"B2\""),
            "{out}"
        );
        // B2 had no prior thread, so the inverse restores an EMPTY thread
        // (a `comment.replace-thread` with no messages = pure removal).
        assert!(
            out.contains("\"inverse\":{\"verb\":\"comment.replace-thread\"")
                && out.contains("\"messages\":[]"),
            "{out}"
        );
        assert!(
            out.contains("\"undoSteps\":0"),
            "not-on-the-stack verbs still carry undoSteps, for a uniform Task 7 contract: {out}"
        );
        let list = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        assert!(
            list.contains("\"author\":\"Ana\"") && list.contains("\"text\":\"Check this\""),
            "{list}"
        );

        // Revert via the declared inverse (the internal replace-thread verb).
        let inv = s.ctl(
            r#"{"verb":"comment.replace-thread","args":{"sheet":0,"ref":"B2","messages":[]}}"#,
        );
        assert!(inv.contains("\"ok\":true"), "{inv}");
        let list = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        assert!(list.contains("\"comments\":[]"), "{list}");
    }

    #[test]
    fn ctl_comment_add_rejects_empty_text() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":""}}"#);
        assert!(out.contains("\"ok\":false"), "{out}");
    }

    #[test]
    fn ctl_comment_remove_reports_removed_and_a_working_inverse() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":"Hi","author":"Bo"}}"#);
        let out = s.ctl(r#"{"verb":"comment.remove","args":{"ref":"A1"}}"#);
        assert!(out.contains("\"removed\":true"), "{out}");
        assert!(
            out.contains("\"inverse\":{\"verb\":\"comment.replace-thread\"")
                && out.contains("\"text\":\"Hi\"")
                && out.contains("\"author\":\"Bo\""),
            "{out}"
        );
        assert!(out.contains("\"undoSteps\":0"), "{out}");
        let list = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        assert!(list.contains("\"comments\":[]"), "{list}");

        // Apply the declared inverse (the internal replace-thread verb).
        let inv = s.ctl(
            r#"{"verb":"comment.replace-thread","args":{"sheet":0,"ref":"A1","messages":[{"author":"Bo","text":"Hi"}]}}"#,
        );
        assert!(inv.contains("\"ok\":true"), "{inv}");
        let list = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        assert!(
            list.contains("\"author\":\"Bo\"") && list.contains("\"text\":\"Hi\""),
            "{list}"
        );
    }

    #[test]
    fn ctl_comment_remove_reports_false_and_no_inverse_when_nothing_there() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"comment.remove","args":{"ref":"A1"}}"#);
        assert!(
            out.contains("\"removed\":false")
                && !out.contains("\"inverse\"")
                && out.contains("\"undoSteps\":0"),
            "{out}"
        );
    }

    #[test]
    fn ctl_comment_add_and_remove_are_not_on_the_undo_stack() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t5\t0\thello"); // pushes one undo entry
        s.ctl(r#"{"verb":"comment.add","args":{"ref":"B2","text":"note"}}"#);
        s.dispatch("undo"); // must revert the cell edit, not the comment
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"\""),
            "the cell edit must be the thing undone: {v}"
        );
        let list = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        assert!(
            list.contains("\"comments\":[{"),
            "the comment must still be present, untouched by undo: {list}"
        );
    }

    // -- range.set: one wasm-undo-stack group ----------------------------

    #[test]
    fn ctl_range_set_writes_a_block_atomically_as_one_undo_group() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out =
            s.ctl(r#"{"verb":"range.set","args":{"start":"D1","rows":[["1","2"],["3","4"]]}}"#);
        assert!(
            out.contains("\"set\":4") && out.contains("\"undoSteps\":1"),
            "{out}"
        );
        s.dispatch("select\t0\t3");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"1\""), "{v}");
        s.dispatch("select\t1\t4");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"4\""), "{v}");

        s.dispatch("undo"); // one undo restores all 4 cells
        s.dispatch("select\t0\t3");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"\""),
            "one undo must clear the whole block: {v}"
        );
        s.dispatch("select\t1\t4");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"\""), "{v}");
    }

    #[test]
    fn ctl_range_set_is_atomic_bad_formula_touches_nothing() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let undo_depth_before = s.undo.len();
        let out = s.ctl(r#"{"verb":"range.set","args":{"start":"D1","rows":[["1","=SUM(("]]}}"#);
        assert!(
            out.contains("\"ok\":false") && out.contains("formula error at E1"),
            "{out}"
        );
        assert_eq!(
            s.undo.len(),
            undo_depth_before,
            "a rejected batch must not touch the undo stack"
        );
        s.dispatch("select\t0\t3");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"\""),
            "nothing in the batch should have been written: {v}"
        );
    }

    // -- sheet.add: a true wasm-undo-stack entry (the SheetAdd precedent) --

    #[test]
    fn ctl_sheet_add_is_a_true_wasm_undo_entry_and_does_not_disturb_the_view() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"sheet.add","args":{"name":"Data"}}"#);
        assert!(
            out.contains("\"sheet\":1")
                && out.contains("\"name\":\"Data\"")
                && out.contains("\"undoSteps\":1"),
            "{out}"
        );
        let v = s.view_json(None);
        assert!(
            v.contains("\"active\":0") && v.contains("Apple"),
            "sheet.add must not yank the active view: {v}"
        );
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Data\"]"), "{v}");

        s.dispatch("undo"); // the declared mechanism: one dispatch("undo")
        let v = s.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\"]"),
            "one undo must remove the added sheet: {v}"
        );
    }

    #[test]
    fn ctl_sheet_add_defaults_name_and_dedupes() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"sheet.add","args":{}}"#);
        assert!(out.contains("\"name\":\"Sheet\""), "{out}");
        let out2 = s.ctl(r#"{"verb":"sheet.add","args":{}}"#);
        assert!(out2.contains("\"name\":\"Sheet 2\""), "{out2}");
    }

    // -- sheet.remove: clears history, reports a LOSSLESS restore inverse --

    #[test]
    fn ctl_sheet_remove_clears_history_and_reports_a_restore_inverse() {
        let mut s = two_sheet_session();
        s.dispatch("set\t5\t0\thello"); // pushes one undo entry, must be wiped
        let out = s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#);
        assert!(out.contains("\"removed\":true"), "{out}");
        assert!(
            out.contains(
                "\"inverse\":{\"verb\":\"sheet.restore-removed\",\"args\":{\"name\":\"Sheet2\"}}"
            ),
            "{out}"
        );
        assert!(out.contains("\"undoSteps\":0"), "{out}");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");

        // History was cleared, not merely left un-added-to.
        s.dispatch("undo");
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"hello\""),
            "sheet.remove must clear existing undo history: {v}"
        );
    }

    #[test]
    fn ctl_sheet_remove_restore_round_trips_cells_formulas_name_comments_and_defined_names() {
        let mut s = two_sheet_session();
        s.active = 1; // Sheet2
        s.dispatch("set\t0\t0\t10");
        s.dispatch("set\t0\t1\t=A1*2");
        s.active = 0;
        s.ctl(
            r#"{"verb":"comment.add","args":{"ref":"A1","text":"Check this","author":"Ana","sheet":1}}"#,
        );
        // A sheet-scoped defined name — no ctl verb creates these (out of
        // wave-1 scope), so push it directly, same as a load from a real
        // .xlsx would populate `workbook.defined_names`.
        s.pkg.workbook.defined_names.push(DefinedName {
            name: "MyRange".to_string(),
            scope: Some(1), // scoped to Sheet2
            formula: "A1:B2".to_string(),
        });

        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#);
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");
        assert_eq!(
            s.pkg.workbook.defined_name("MyRange", 0),
            None,
            "the scoped name must vanish along with its sheet"
        );

        let out = s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        assert!(
            out.contains("\"sheet\":1") && out.contains("\"name\":\"Sheet2\""),
            "{out}"
        );
        assert!(
            out.contains("\"inverse\":{\"verb\":\"sheet.remove\",\"args\":{\"sheet\":1}}"),
            "the restore's own inverse must target the NEW index, for redo: {out}"
        );
        assert!(out.contains("\"undoSteps\":0"), "{out}");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Sheet2\"]"), "{v}");

        s.active = 1;
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(cell.contains("\"value\":10"), "{cell}");
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B1"}}"#);
        assert!(
            cell.contains("\"formula\":\"=A1*2\"") && cell.contains("\"value\":20"),
            "the formula AND its recalculated value must both survive: {cell}"
        );
        let list = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        assert!(
            list.contains("\"sheet\":1")
                && list.contains("\"ref\":\"A1\"")
                && list.contains("\"author\":\"Ana\"")
                && list.contains("\"text\":\"Check this\""),
            "the comment must be restored too, not silently dropped: {list}"
        );
        assert_eq!(
            s.pkg.workbook.defined_name("MyRange", 1),
            Some("A1:B2"),
            "the defined name must resolve again, scoped to the restored sheet's NEW index"
        );
    }

    #[test]
    fn sheet_remove_restore_writes_the_restored_names_on_save() {
        // #731: Sheet1, Report, Tail. Report has a print area, a scoped name
        // and an autoFilter with its `_FilterDatabase`; Tail a name of its own.
        let mut pkg = new_xlsx();
        pkg.add_sheet("Report");
        pkg.add_sheet("Tail");
        let names = concat!(
            r#"<definedNames>"#,
            r#"<definedName name="_xlnm._FilterDatabase" localSheetId="1" hidden="1">Report!$A$1:$B$5</definedName>"#,
            r#"<definedName name="_xlnm.Print_Area" localSheetId="1">Report!$A$1:$D$20</definedName>"#,
            r#"<definedName name="Mine" localSheetId="1">Report!$B$2</definedName>"#,
            r#"<definedName name="Mine" localSheetId="2">Tail!$C$3</definedName>"#,
            r#"</definedNames>"#,
        );
        let wb = String::from_utf8_lossy(pkg.part("xl/workbook.xml").unwrap())
            .replace("</sheets>", &format!("</sheets>{names}"));
        pkg.set_part("xl/workbook.xml", wb.into_bytes());
        let report_part = "xl/worksheets/sheet2.xml";
        let ws = String::from_utf8_lossy(pkg.part(report_part).expect("Report's part"))
            .replace("<sheetData/>", r#"<sheetData/><autoFilter ref="A1:B5"/>"#);
        pkg.set_part(report_part, ws.into_bytes());
        // What loading that part gives, so the save keeps the element.
        pkg.workbook.sheets[1].auto_filter = Some(gridcore::sheet::SheetAutoFilter {
            range: (0, 0, 4, 1),
            columns: Vec::new(),
        });
        let mut s = Session::open(&save_xlsx(&pkg)).expect("open");
        assert!(s.pkg.workbook.sheets[1].auto_filter.is_some());
        assert_eq!(
            s.pkg.workbook.defined_name("_xlnm._FilterDatabase", 1),
            Some("Report!$A$1:$B$5")
        );

        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Report"}}"#);
        let out = s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        assert!(out.contains("\"sheet\":2"), "restored at the end: {out}");

        let re = load_xlsx(&s.save()).expect("saved file reloads");
        let scoped = |i: usize| {
            let mut v: Vec<(String, String)> = re
                .workbook
                .defined_names
                .iter()
                .filter(|d| d.scope == Some(i))
                .map(|d| (d.name.clone(), d.formula.clone()))
                .collect();
            v.sort();
            v
        };
        // The restored sheet's names, scoped to its new index; no
        // `_FilterDatabase`, since its fresh part has no autoFilter to back.
        assert_eq!(
            scoped(2),
            vec![
                ("Mine".to_string(), "Report!$B$2".to_string()),
                (
                    "_xlnm.Print_Area".to_string(),
                    "Report!$A$1:$D$20".to_string()
                ),
            ]
        );
        assert_eq!(
            scoped(1),
            vec![("Mine".to_string(), "Tail!$C$3".to_string())]
        );
        let wb = String::from_utf8_lossy(re.part("xl/workbook.xml").unwrap()).into_owned();
        assert!(!wb.contains("_FilterDatabase"), "{wb}");
        assert_eq!(wb.matches("<definedName ").count(), 3, "{wb}");
    }

    #[test]
    fn ctl_sheet_restore_removed_errors_when_nothing_is_stashed() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        assert!(out.contains("nothing to restore"), "{out}");
    }

    #[test]
    fn ctl_sheet_restore_removed_stash_is_single_slot_second_removal_overwrites_first() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"sheet.add","args":{"name":"A"}}"#);
        s.ctl(r#"{"verb":"sheet.add","args":{"name":"B"}}"#);
        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"A"}}"#); // stash = A
        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"B"}}"#); // stash = B, overwrites A

        let out = s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        assert!(
            out.contains("\"name\":\"B\""),
            "the SECOND removal's stash wins: {out}"
        );
        let v = s.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\",\"B\"]"),
            "A is gone for good — only the latest removal is restorable: {v}"
        );

        // The stash is now empty again (restore takes/clears it).
        let out2 = s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        assert!(out2.contains("nothing to restore"), "{out2}");
    }

    #[test]
    fn ctl_sheet_restore_removed_adjusts_active_index_removed_before_active_case() {
        // sheets = [Sheet1, Sheet2, Sheet3], active = Sheet3 (index 2).
        // Removing Sheet2 (index 1, BELOW active) decrements active to 1,
        // still correctly pointing at Sheet3. Restoring (append-only) must
        // NOT touch active again — it already correctly tracks Sheet3.
        let mut s = two_sheet_session();
        s.ctl(r#"{"verb":"sheet.add","args":{"name":"Sheet3"}}"#);
        s.active = 2;
        s.cur = (0, 0);
        s.anchor = None;

        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#);
        let v = s.view_json(None);
        assert!(v.contains("\"active\":1"), "{v}"); // Sheet3 shifted down to index 1

        s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        let v = s.view_json(None);
        assert!(
            v.contains("\"active\":1"),
            "restoring (append-only) must not move active off Sheet3: {v}"
        );
        assert!(
            v.contains("\"sheets\":[\"Sheet1\",\"Sheet3\",\"Sheet2\"]"),
            "Sheet2 comes back at the END, not spliced into its old middle slot: {v}"
        );
    }

    #[test]
    fn ctl_sheet_restore_removed_adjusts_active_index_removed_active_case() {
        let mut s = two_sheet_session();
        s.active = 1; // Sheet2 is the active/visible one
        s.cur = (2, 1);

        let out = s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#);
        assert!(out.contains("\"removed\":true"), "{out}");
        let v = s.view_json(None);
        assert!(v.contains("\"active\":0"), "{v}");

        let out = s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        assert!(out.contains("\"sheet\":1"), "{out}");
        let v = s.view_json(None);
        assert!(
            v.contains("\"active\":1") && v.contains("\"ref\":\"A1\""),
            "restoring the sheet that WAS active brings the user back to it, freshly: {v}"
        );
    }

    #[test]
    fn ctl_sheet_restore_removed_inverse_chains_into_a_working_redo() {
        let mut s = two_sheet_session();
        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#);
        let restore = s.ctl(r#"{"verb":"sheet.restore-removed","args":{}}"#);
        assert!(restore.contains("\"ok\":true"), "{restore}");

        // Apply the restore's own declared inverse: sheet.remove again.
        let redo = s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":1}}"#);
        assert!(redo.contains("\"removed\":true"), "{redo}");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");
    }

    /// Extract the `inverse` object (a valid `{"verb":…,"args":…}` request)
    /// out of a ctl reply envelope, by balanced-brace scanning — so tests
    /// replay exactly the inverse the code produced, not a hand-written copy.
    fn extract_inverse(reply: &str) -> String {
        let key = "\"inverse\":";
        let start = reply.find(key).expect("reply carries an inverse") + key.len();
        let bytes = reply.as_bytes();
        let (mut depth, mut in_str, mut esc) = (0usize, false, false);
        for (i, &b) in bytes[start..].iter().enumerate() {
            let ch = b as char;
            if in_str {
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == '"' {
                    in_str = false;
                }
            } else {
                match ch {
                    '"' => in_str = true,
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return reply[start..start + i + 1].to_string();
                        }
                    }
                    _ => {}
                }
            }
        }
        panic!("unbalanced inverse object in reply: {reply}");
    }

    // -- Fix 1: identity-guarded sheet restore ---------------------------

    #[test]
    fn ctl_sheet_restore_identity_guard_rejects_a_stash_clobber_interleave() {
        // The exact CRITICAL interleave. Agent removes A (stash=A); agent
        // imports a CSV (a new sheet); then the import's OWN inverse
        // (sheet.remove of that imported sheet) OVERWRITES the single-slot
        // stash. Applying A's restore inverse must now ERROR on the name
        // mismatch, never silently resurrect the imported sheet in A's place.
        let mut s = two_sheet_session(); // [Sheet1, Sheet2] — Sheet2 is our "A"
        let removed = s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#);
        assert!(
            removed.contains(
                "\"inverse\":{\"verb\":\"sheet.restore-removed\",\"args\":{\"name\":\"Sheet2\"}}"
            ),
            "sheet.remove's inverse must name the removed sheet: {removed}"
        );
        // Agent import-csv -> a new sheet "Imp"; the stash is still Sheet2.
        let imported =
            s.ctl(r#"{"verb":"sheet.import-csv","args":{"name":"Imp","text":"x,y\n1,2"}}"#);
        assert!(imported.contains("\"name\":\"Imp\""), "{imported}");
        // Apply the IMPORT's inverse (sheet.remove of the imported sheet) —
        // this clobbers the stash, replacing Sheet2 with Imp.
        let clobber = s.ctl(&extract_inverse(&imported));
        assert!(clobber.contains("\"removed\":true"), "{clobber}");
        // Now apply A's (Sheet2's) restore inverse: the names no longer match.
        let restore = s.ctl(&extract_inverse(&removed));
        assert!(
            restore.contains("\"ok\":false") && restore.contains("restore mismatch"),
            "the clobbered stash must be detected, not mis-restored: {restore}"
        );
        // And nothing was restored — only Sheet1 remains.
        let v = s.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\"]"),
            "a rejected restore must not resurrect the wrong sheet: {v}"
        );
    }

    #[test]
    fn ctl_sheet_restore_removed_errors_on_a_name_collision() {
        // A same-name `sheet.add` between removal and restore would make the
        // restore mint a DUPLICATE name (add_sheet doesn't dedupe) — refuse.
        let mut s = two_sheet_session(); // [Sheet1, Sheet2]
        s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#); // stash = Sheet2
        s.ctl(r#"{"verb":"sheet.add","args":{"name":"Sheet2"}}"#); // a new, live Sheet2
        let out = s.ctl(r#"{"verb":"sheet.restore-removed","args":{"name":"Sheet2"}}"#);
        assert!(
            out.contains("\"ok\":false") && out.contains("already exists"),
            "restoring onto an existing same-name sheet must error, not duplicate: {out}"
        );
        // Still exactly one Sheet2 — the rejected restore added nothing.
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Sheet2\"]"), "{v}");
    }

    // -- Fix 2: faithful comment inverses (both directions) --------------

    #[test]
    fn ctl_comment_add_inverse_preserves_a_pre_existing_thread() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        // A pre-existing two-message thread on A1.
        s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":"first","author":"Ana"}}"#);
        s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":"second","author":"Bob"}}"#);
        // The add whose inverse we test: a reply appended to that thread.
        let out =
            s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":"third","author":"Cid"}}"#);
        // Its inverse restores the PRE-ADD thread (first, second) — NOT a wipe.
        assert!(
            out.contains("\"inverse\":{\"verb\":\"comment.replace-thread\"")
                && out.contains("\"text\":\"first\"")
                && out.contains("\"text\":\"second\"")
                && !out.contains("\"messages\":[]"),
            "the add-reply inverse must carry the prior thread, not wipe the ref: {out}"
        );
        // Apply the exact inverse the code produced.
        let inv = s.ctl(&extract_inverse(&out));
        assert!(inv.contains("\"ok\":true"), "{inv}");
        // The original two messages survive, in order; the appended reply is gone.
        let list = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        let (p1, p2) = (list.find("first"), list.find("second"));
        assert!(
            p1.is_some() && p2.is_some() && p1 < p2 && !list.contains("third"),
            "undoing the reply must leave the original thread intact and ordered: {list}"
        );
    }

    #[test]
    fn ctl_comment_remove_inverse_restores_every_message_in_order() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        // A three-message thread on A1.
        s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":"m1","author":"Ana"}}"#);
        s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":"m2","author":"Bob"}}"#);
        s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":"m3","author":"Cid"}}"#);
        // Remove the whole ref; the inverse must carry ALL three messages.
        let out = s.ctl(r#"{"verb":"comment.remove","args":{"ref":"A1"}}"#);
        assert!(out.contains("\"removed\":true"), "{out}");
        assert!(
            out.contains("\"text\":\"m1\"")
                && out.contains("\"text\":\"m2\"")
                && out.contains("\"text\":\"m3\""),
            "the remove inverse must carry the whole thread, not just its first message: {out}"
        );
        assert!(
            s.ctl(r#"{"verb":"comment.list","args":{}}"#)
                .contains("\"comments\":[]")
        );
        // Apply the inverse: all three come back, in order.
        let inv = s.ctl(&extract_inverse(&out));
        assert!(inv.contains("\"ok\":true"), "{inv}");
        let list = s.ctl(r#"{"verb":"comment.list","args":{}}"#);
        let (a, b, c) = (list.find("m1"), list.find("m2"), list.find("m3"));
        assert!(
            a.is_some() && a < b && b < c,
            "every message must be restored, in original order: {list}"
        );
    }

    #[test]
    fn ctl_comment_replace_thread_inverse_chains_into_a_working_redo() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"comment.add","args":{"ref":"A1","text":"keep","author":"Ana"}}"#);
        // Undo the removal → restores the thread; its reply carries the redo.
        let removed = s.ctl(r#"{"verb":"comment.remove","args":{"ref":"A1"}}"#);
        let restore = s.ctl(&extract_inverse(&removed));
        assert!(restore.contains("\"ok\":true"), "{restore}");
        assert!(
            s.ctl(r#"{"verb":"comment.list","args":{}}"#)
                .contains("\"text\":\"keep\""),
            "the thread must be back after undo"
        );
        // Redo = apply the restore's OWN inverse → the thread is removed again.
        let redo = s.ctl(&extract_inverse(&restore));
        assert!(redo.contains("\"ok\":true"), "{redo}");
        assert!(
            s.ctl(r#"{"verb":"comment.list","args":{}}"#)
                .contains("\"comments\":[]"),
            "redoing the removal must clear the thread again"
        );
    }

    #[test]
    fn ctl_comment_replace_thread_is_not_a_public_verb() {
        // Internal-only: reached solely via the host-orchestrated inverse. It
        // IS routed in `ctl`, but must never appear on `wasmVerbs`/MCP/docs —
        // that omission is what keeps external agents out (asserted in Task 7).
        // Here we only pin that its own reply shape is inverse-bearing.
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(
            r#"{"verb":"comment.replace-thread","args":{"sheet":0,"ref":"A1","messages":[{"author":"Ana","text":"x"}]}}"#,
        );
        assert!(
            out.contains("\"ok\":true")
                && out.contains("\"inverse\":{\"verb\":\"comment.replace-thread\"")
                && out.contains("\"undoSteps\":0"),
            "{out}"
        );
    }

    #[test]
    fn ctl_sheet_remove_errors_on_the_last_sheet() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":0}}"#);
        assert!(out.contains("cannot remove the last sheet"), "{out}");
    }

    #[test]
    fn ctl_sheet_remove_requires_an_explicit_sheet_arg() {
        let mut s = two_sheet_session();
        let out = s.ctl(r#"{"verb":"sheet.remove","args":{}}"#);
        assert!(out.contains("needs a 'sheet'"), "{out}");
    }

    #[test]
    fn ctl_sheet_remove_leaves_the_viewport_alone_when_inactive_sheet_removed() {
        let mut s = two_sheet_session();
        s.dispatch("select\t2\t1"); // move the cursor on the active (Sheet1) view
        let out = s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#);
        assert!(out.contains("\"removed\":true"), "{out}");
        let v = s.view_json(None);
        assert!(
            v.contains("\"ref\":\"B3\""),
            "removing an inactive sheet must not move the cursor: {v}"
        );
    }

    #[test]
    fn ctl_sheet_remove_resets_the_viewport_when_the_active_sheet_is_removed() {
        let mut s = two_sheet_session();
        s.active = 1; // Sheet2 is now the active/visible one
        s.cur = (2, 1);
        let out = s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":"Sheet2"}}"#);
        assert!(out.contains("\"removed\":true"), "{out}");
        let v = s.view_json(None);
        assert!(
            v.contains("\"ref\":\"A1\""),
            "removing the active sheet must reset the cursor: {v}"
        );
    }

    // -- sheet.rename: one wasm-undo-stack (Structural) group -------------

    #[test]
    fn ctl_sheet_rename_is_one_undo_group_and_rewrites_refs() {
        let mut s = two_sheet_session();
        s.active = 1;
        s.dispatch("set\t0\t0\t=Sheet1!A1");
        s.active = 0;
        let out = s.ctl(r#"{"verb":"sheet.rename","args":{"sheet":0,"name":"Renamed"}}"#);
        assert!(
            out.contains("\"name\":\"Renamed\"") && out.contains("\"undoSteps\":1"),
            "{out}"
        );
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Renamed\",\"Sheet2\"]"), "{v}");
        s.active = 1;
        s.dispatch("select\t0\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=Renamed!A1\""),
            "the cross-sheet ref must rewrite: {v}"
        );

        s.dispatch("undo"); // one undo restores both the name and the formula
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Sheet2\"]"), "{v}");
        s.dispatch("select\t0\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=Sheet1!A1\""),
            "one undo must restore the original ref too: {v}"
        );
    }

    #[test]
    fn ctl_sheet_rename_rejects_invalid_names() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"sheet.rename","args":{"sheet":0,"name":"a/b"}}"#);
        assert!(out.contains("invalid sheet name"), "{out}");
    }

    // -- row.* / col.*: one wasm-undo-stack (Structural) group -------------

    #[test]
    fn ctl_row_insert_shifts_refs_and_is_one_undo_group() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"row.insert","args":{"at":1,"count":1}}"#);
        assert!(
            out.contains("\"inserted\":1") && out.contains("\"undoSteps\":1"),
            "{out}"
        );
        s.dispatch("select\t4\t1");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=SUM(B1:B4)\""),
            "refs must shift: {v}"
        );

        s.dispatch("undo");
        s.dispatch("select\t3\t1");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=SUM(B1:B3)\""),
            "one undo restores refs: {v}"
        );
    }

    #[test]
    fn ctl_col_delete_reports_count_and_is_one_undo_group() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"col.delete","args":{"at":0,"count":1}}"#);
        assert!(
            out.contains("\"deleted\":1") && out.contains("\"undoSteps\":1"),
            "{out}"
        );
        let v = s.view_json(None);
        assert!(!v.contains("Apple") && v.contains("3.75"), "{v}");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(
            v.contains("Apple"),
            "one undo restores the deleted column: {v}"
        );
    }

    #[test]
    fn ctl_row_delete_rejects_zero_count() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"row.delete","args":{"at":0,"count":0}}"#);
        assert!(out.contains("'count' must be at least 1"), "{out}");
    }

    // -- cell.format: Task 3's bucket A carries over (true wasm-undo-stack) -
    //
    // NOTE: these tests previously PINNED a pre-existing gridcore bug (a
    // spurious `"numFmt":"General"` leaking into every `cell.get` on any
    // real loaded workbook — see task-4-report.md's original "CRITICAL
    // FINDING" section for the full root-cause writeup). That bug is now
    // FIXED in `gridcore::format::xf_format_fields` (see that function's
    // doc comment: the `numFmt` gate now keys off `Xf::numfmt !=
    // NumFmt::General` rather than a raw `Xf::code` comparison), so these
    // tests below assert the CORRECT contract — an unstyled cell has no
    // `format` key at all.

    #[test]
    fn ctl_cell_format_bold_and_fill_is_one_undo_group_and_undo_clears_the_format_key() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(
            r##"{"verb":"cell.format","args":{"range":"A1:B2","patch":{"bold":true,"fillColor":"#FFFF00"}}}"##,
        );
        assert!(
            out.contains("\"formatted\":4") && out.contains("\"undoSteps\":1"),
            "{out}"
        );

        // Value preserved, format visible per-cell via cell.get.
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A2"}}"#);
        assert!(cell.contains("\"text\":\"Apple\""), "{cell}");
        assert!(
            cell.contains(r##""format":{"bold":true,"fillColor":"#FFFF00"}"##),
            "{cell}"
        );

        s.dispatch("undo"); // the declared mechanism: ONE dispatch("undo")
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A2"}}"#);
        assert!(
            !cell.contains("\"format\""),
            "one undo must clear the format key on every cell in the range: {cell}"
        );
        assert!(
            cell.contains("\"text\":\"Apple\""),
            "value untouched: {cell}"
        );
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B2"}}"#);
        assert!(!cell.contains("\"format\""), "{cell}");
    }

    #[test]
    fn ctl_cell_format_rejects_a_range_over_the_cap_and_touches_nothing() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let undo_before = s.undo.len();
        // A1:A1048576 — the whole column — vastly exceeds the cap; must be
        // rejected BEFORE materializing a Cell per coordinate.
        let out =
            s.ctl(r#"{"verb":"cell.format","args":{"range":"A1:A1048576","patch":{"bold":true}}}"#);
        assert!(
            out.contains("\"ok\":false")
                && out.contains(&format!(
                    "cell.format: range too large (limit {} cells)",
                    gridcore::format::CELL_FORMAT_CAP
                )),
            "{out}"
        );
        assert_eq!(s.undo.len(), undo_before, "a rejected range must not apply");
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(!cell.contains("\"format\""), "{cell}");
    }

    #[test]
    fn ctl_cell_format_at_the_cap_still_works() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        // A 50x100 rectangle lands exactly at the cap: allowed, and every
        // cell in it gets formatted.
        let cols: u32 = 50;
        let rows: u64 = gridcore::format::CELL_FORMAT_CAP / u64::from(cols);
        assert_eq!(rows * u64::from(cols), gridcore::format::CELL_FORMAT_CAP);
        let range = format!("A1:{}{rows}", gridcore::sheet::col_name(cols - 1));
        let out = s.ctl(&format!(
            r#"{{"verb":"cell.format","args":{{"range":"{range}","patch":{{"bold":true}}}}}}"#
        ));
        assert!(
            out.contains(&format!(
                "\"formatted\":{}",
                gridcore::format::CELL_FORMAT_CAP
            )),
            "{out}"
        );
    }

    #[test]
    fn ctl_cell_format_preserves_a_formula_and_its_recalculated_value() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"cell.format","args":{"range":"B4","patch":{"bold":true}}}"#);
        assert!(out.contains("\"formatted\":1"), "{out}");
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B4"}}"#);
        assert!(cell.contains("\"formula\":\"=SUM(B1:B3)\""), "{cell}");
        assert!(cell.contains("\"value\":3.75"), "{cell}");
        assert!(cell.contains("\"format\":{\"bold\":true}"), "{cell}");
    }

    #[test]
    fn ctl_cell_format_align_and_numfmt_round_trip_through_cell_get() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(
            r#"{"verb":"cell.format","args":{"range":"B2","patch":{"numFmt":"0.00","italic":true,"align":"right"}}}"#,
        );
        let out = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B2"}}"#);
        assert!(
            out.contains(r#""format":{"numFmt":"0.00","italic":true,"align":"right"}"#),
            "{out}"
        );
    }

    #[test]
    fn ctl_cell_get_reports_no_format_key_for_an_unstyled_cell() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(!out.contains("\"format\""), "{out}");
    }

    #[test]
    fn ctl_cell_format_reset_to_general_numfmt_echoes_nothing() {
        // An agent explicitly resetting numFmt to General is, semantically,
        // resetting to the default — cell.get must not echo it afterward
        // (gridcore::format::xf_format_fields's documented rule).
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"cell.format","args":{"range":"A1","patch":{"numFmt":"General"}}}"#);
        let out = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(!out.contains("\"format\""), "{out}");
    }

    #[test]
    fn ctl_format_read_back_is_scoped_to_cell_get_only() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"cell.format","args":{"range":"A1","patch":{"bold":true}}}"#);

        let get = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(get.contains("\"format\":{\"bold\":true}"), "{get}");

        let read = s.ctl(r#"{"verb":"sheet.read","args":{"range":"A1"}}"#);
        assert!(
            !read.contains("\"format\""),
            "sheet.read must stay format-less: {read}"
        );

        let find = s.ctl(r#"{"verb":"find","args":{"query":"Item"}}"#);
        assert!(
            !find.contains("\"format\""),
            "find must stay format-less: {find}"
        );

        let set = s.ctl(r#"{"verb":"cell.set","args":{"ref":"A1","text":"Item"}}"#);
        assert!(
            !set.contains("\"format\""),
            "cell.set's own reply must stay format-less: {set}"
        );
        // ...even though the style survived the rewrite (still bold via cell.get).
        let get = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(get.contains("\"format\":{\"bold\":true}"), "{get}");
    }

    #[test]
    fn ctl_cell_format_reflects_in_the_view_and_marks_dirty() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"cell.format","args":{"range":"A2","patch":{"bold":true}}}"#);
        let v = s.view_json(None);
        assert!(v.contains("\"dirty\":true"), "{v}");
        assert!(
            v.contains("\"r\":1,\"c\":0") && v.contains("\"b\":1"),
            "A2's bold must repaint in the viewport: {v}"
        );
    }

    #[test]
    fn ctl_cell_format_empty_patch_errors_and_applies_nothing() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let undo_before = s.undo.len();
        let out = s.ctl(r#"{"verb":"cell.format","args":{"range":"A1","patch":{}}}"#);
        assert!(
            out.contains("\"ok\":false") && out.contains("patch needs at least one key"),
            "{out}"
        );
        assert_eq!(s.undo.len(), undo_before, "a rejected patch must not apply");
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(!cell.contains("\"format\""), "{cell}");
    }

    #[test]
    fn ctl_cell_format_missing_patch_key_errors() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"cell.format","args":{"range":"A1"}}"#);
        assert!(
            out.contains("cell.format needs a 'patch'") && !out.contains("'patch' object"),
            "a missing key and a non-object patch must produce distinct messages: {out}"
        );
    }

    #[test]
    fn ctl_cell_format_rejects_a_non_object_patch() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"cell.format","args":{"range":"A1","patch":"bold"}}"#);
        assert!(out.contains("cell.format needs a 'patch' object"), "{out}");
    }

    #[test]
    fn ctl_cell_format_unknown_key_names_it_and_applies_nothing() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"cell.format","args":{"range":"A1","patch":{"wrap":true}}}"#);
        assert!(
            out.contains("\"ok\":false") && out.contains("unknown patch key 'wrap'"),
            "{out}"
        );
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(!cell.contains("\"format\""), "{cell}");
    }

    #[test]
    fn ctl_cell_format_bad_num_fmt_errors_and_applies_nothing() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(
            r#"{"verb":"cell.format","args":{"range":"A1","patch":{"numFmt":"[[[not a format"}}}"#,
        );
        assert!(
            out.contains("\"ok\":false") && out.contains("bad numFmt code"),
            "{out}"
        );
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(!cell.contains("\"format\""), "{cell}");
    }

    #[test]
    fn ctl_cell_format_bad_color_errors_and_applies_nothing() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out =
            s.ctl(r#"{"verb":"cell.format","args":{"range":"A1","patch":{"fontColor":"red"}}}"#);
        assert!(
            out.contains("\"ok\":false") && out.contains("bad color"),
            "{out}"
        );
        let cell = s.ctl(r#"{"verb":"cell.get","args":{"ref":"A1"}}"#);
        assert!(!cell.contains("\"format\""), "{cell}");
    }

    // -- col.width: NOT on the undo stack; inverse carries prior width -----

    #[test]
    fn ctl_col_width_sets_and_the_inverse_restores_and_chains_a_working_redo() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let undo_before = s.undo.len();
        let prior = s.pkg.workbook.sheets[0].col_width(2);
        let out = s.ctl(r#"{"verb":"col.width","args":{"col":"C","width":20}}"#);
        assert!(
            out.contains("\"col\":2")
                && out.contains("\"width\":20")
                && out.contains("\"undoSteps\":0"),
            "{out}"
        );
        assert_eq!(
            s.undo.len(),
            undo_before,
            "col.width must NOT be on the undo stack"
        );
        assert_eq!(s.pkg.workbook.sheets[0].col_width(2), 20.0);
        assert!(s.dirty, "col.width must still mark the session dirty");

        // Applying the declared inverse restores the PRIOR width.
        let inv_reply = s.ctl(&extract_inverse(&out));
        assert_eq!(s.pkg.workbook.sheets[0].col_width(2), prior);
        assert_eq!(
            s.undo.len(),
            undo_before,
            "the inverse call must not touch the undo stack either"
        );

        // The inverse's OWN reply chains a working redo (back to 20).
        let redo_inv = extract_inverse(&inv_reply);
        s.ctl(&redo_inv);
        assert_eq!(s.pkg.workbook.sheets[0].col_width(2), 20.0);
    }

    #[test]
    fn ctl_col_width_accepts_a_0_based_index_too() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"col.width","args":{"col":4,"width":15}}"#);
        assert!(
            out.contains("\"col\":4") && out.contains("\"width\":15"),
            "{out}"
        );
        assert_eq!(s.pkg.workbook.sheets[0].col_width(4), 15.0);
    }

    #[test]
    fn ctl_col_width_rejects_a_huge_numeric_col() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"col.width","args":{"col":99999999,"width":20}}"#);
        assert!(
            out.contains("\"ok\":false") && out.contains("bad column '99999999'"),
            "{out}"
        );
    }

    #[test]
    fn ctl_col_width_accepts_a_digit_string_col_and_bounds_it() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        // A schema-conforming string index ("5"), not just a JSON number.
        let out = s.ctl(r#"{"verb":"col.width","args":{"col":"5","width":12}}"#);
        assert!(
            out.contains("\"col\":5") && out.contains("\"width\":12"),
            "{out}"
        );
        assert_eq!(s.pkg.workbook.sheets[0].col_width(5), 12.0);

        // But the same bound applies as the numeric arm.
        let out2 = s.ctl(r#"{"verb":"col.width","args":{"col":"99999999","width":12}}"#);
        assert!(
            out2.contains("\"ok\":false") && out2.contains("bad column '99999999'"),
            "{out2}"
        );
    }

    #[test]
    fn ctl_col_width_letter_arm_is_unchanged() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"col.width","args":{"col":"Z","width":9}}"#);
        assert!(out.contains("\"col\":25"), "{out}");
        let out2 = s.ctl(r#"{"verb":"col.width","args":{"col":"ZZZZ","width":9}}"#);
        assert!(
            out2.contains("\"ok\":false") && out2.contains("bad column 'ZZZZ'"),
            "{out2}"
        );
    }

    #[test]
    fn ctl_col_width_rejects_non_positive_width() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"col.width","args":{"col":"A","width":0}}"#);
        assert!(out.contains("'width' must be positive"), "{out}");
        let out2 = s.ctl(r#"{"verb":"col.width","args":{"col":"A","width":-5}}"#);
        assert!(out2.contains("'width' must be positive"), "{out2}");
    }

    #[test]
    fn ctl_col_width_rejects_a_malformed_column_letter() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"col.width","args":{"col":"C1","width":20}}"#);
        assert!(out.contains("bad column 'C1'"), "{out}");
    }

    // -- wb.replace-all: one wasm-undo-stack group, every sheet ------------

    #[test]
    fn ctl_wb_replace_all_touches_every_sheet_in_one_undo_group() {
        let mut s = two_sheet_session();
        s.active = 1;
        s.dispatch("set\t0\t0\tApple pie");
        s.active = 0;
        s.undo.clear();
        s.redo.clear();

        let out = s.ctl(r#"{"verb":"wb.replace-all","args":{"query":"Apple","text":"Orange"}}"#);
        assert!(
            out.contains("\"replaced\":2") && out.contains("\"undoSteps\":1"),
            "{out}"
        );
        let v0 = s.view_json(None);
        assert!(v0.contains("Orange") && !v0.contains("Apple"), "{v0}");
        s.active = 1;
        let v1 = s.view_json(None);
        assert!(v1.contains("Orange pie"), "{v1}");

        s.dispatch("undo"); // one undo restores BOTH sheets
        s.active = 0;
        let v0 = s.view_json(None);
        assert!(v0.contains("Apple") && !v0.contains("Orange"), "{v0}");
        s.active = 1;
        let v1 = s.view_json(None);
        assert!(v1.contains("Apple pie"), "{v1}");
    }

    #[test]
    fn ctl_wb_replace_all_rejects_empty_query() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let out = s.ctl(r#"{"verb":"wb.replace-all","args":{"query":"","text":"x"}}"#);
        assert!(out.contains("empty query"), "{out}");
    }

    // -- sheet.import-csv: clears history, exact inverse (sheet.remove) ---

    #[test]
    fn ctl_sheet_import_csv_creates_a_new_sheet_and_clears_history_with_an_exact_inverse() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t5\t0\thello"); // pushes one undo entry, must be wiped
        let out =
            s.ctl(r#"{"verb":"sheet.import-csv","args":{"text":"a,b\n1,2\n","name":"Data"}}"#);
        assert!(out.contains("\"sheet\":1"), "{out}");
        assert!(out.contains("\"name\":\"Data\""), "{out}");
        assert!(
            out.contains("\"rows\":2") && out.contains("\"cols\":2"),
            "{out}"
        );
        assert!(
            out.contains("\"inverse\":{\"verb\":\"sheet.remove\",\"args\":{\"sheet\":1}}"),
            "{out}"
        );
        assert!(out.contains("\"undoSteps\":0"), "{out}");

        s.dispatch("undo"); // history was cleared, not merely un-added-to
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"hello\""),
            "sheet.import-csv must clear existing undo history: {v}"
        );
    }

    #[test]
    fn ctl_sheet_import_csv_twice_yields_two_distinct_sheets() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"sheet.import-csv","args":{"text":"a\n1\n","name":"Data"}}"#);
        let out2 = s.ctl(r#"{"verb":"sheet.import-csv","args":{"text":"b\n2\n","name":"Data"}}"#);
        assert!(out2.contains("\"name\":\"Data 2\""), "{out2}");
        let v = s.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\",\"Data\",\"Data 2\"]"),
            "{v}"
        );
    }

    #[test]
    fn ctl_sheet_import_csv_inverse_removes_exactly_the_new_sheet() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.ctl(r#"{"verb":"sheet.import-csv","args":{"text":"a,b\n1,2\n","name":"Data"}}"#);
        let inv = s.ctl(r#"{"verb":"sheet.remove","args":{"sheet":1}}"#);
        assert!(inv.contains("\"removed\":true"), "{inv}");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");
    }
}
