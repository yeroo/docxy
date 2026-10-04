# Plan v2 — #664 sheet copy/paste: tile, adjust refs, Enter-paste, cut-as-move, skip filtered rows

Issue: `.workbench/issue.md`. Batch #707 (which lists #664) is still pending in the queue; this loop
fixes #664 alone and the PR says `Closes #664` only.

## Changes from v1 (answering the critique)

> 1. "a pending cut moves stale cells, and can overwrite edits made after Ctrl+X ... Add an edit
> generation ... bumped in `mark_sheet_dirty`"

Conceded, it is a data-loss hole. But `mark_sheet_dirty` is not every path: suite/docxy/src has 40
direct `dirty = true` writes against 36 `mark_sheet_dirty()` calls. So v2 uses two checks, both
needed for a cut to stay live:
- an edit generation `u64` on `SheetView`, bumped in `push_undo`/`push_undo_snapshot` (every undoable
  sheet edit) and wherever an undo/redo restores a snapshot, recorded in the clip on a cut;
- at paste time the source sheet at the recorded index still has the recorded name, and the cells now
  in the source rect equal `clip.cells` (when filtered rows are not involved, as cuts never skip).
If either fails, the cut is over: nothing is pasted, the status says the cut was cancelled, the clip
is spent. The moved cells are read from the workbook at paste time (equal to `clip.cells` by the
check). Copies are not tied to the generation (pasting a stale copy loses nothing).
Test: Ctrl+X A1:B1, type 5 into A1, Ctrl+V at A10 → nothing changes, A1 is 5.
Test: Ctrl+X A1:B1, insert a row above row 1, Ctrl+V → nothing changes.

> 2. "Esc must end a pending cut (and copy mode)"

Conceded: in scope. The sheet's `"escape"` arm (not editing) spends the clip. Test for copy and cut.
(While editing, Esc only cancels the edit, as today; Excel also keeps copy mode then.)

> 3. "The harness `clipboard` verb must know about the spent clip ... do it in one place"

Conceded, harness.rs is required. Shape: `GridClip` gains `spent: bool` (a spent clip keeps only
what `clip_still_ours` needs). One helper, `grid_clip_live(&self, now) -> Option<&GridClip>` (still
ours and not spent), decides Ctrl+V, Enter, and `clipboard_app_json`'s report; and a separate check
"the clipboard still holds a spent clip's text" makes the TSV branch paste nothing. The harness unit
test `clipboard_reports_the_clip_the_next_paste_would_use` gets a spent case reporting `none`.

> 4. "Cross-sheet cut: decide how the moved block is rewritten ... Proposal: (b)"

Agreed, (b) decided:
- `edit::move_refs(wb, src_sheet, rect, dst_sheet, (dr, dc))` rewrites every formula in the workbook
  *except the cells inside the source rect on the source sheet* (they are being moved): cell
  formulas, defined names, CF/DV rule formulas. A ref (or range with both corners) wholly inside the
  rect on the source sheet is shifted by (dr, dc), absolute parts too, and re-qualified with the
  destination sheet when it differs (unqualified stays unqualified when home == dst).
- Each moved cell's formula goes through `formula::move_block_formula(src, src_name, dst_name, rect,
  dr, dc)`: refs wholly inside the rect are shifted and stay unqualified when dst is the home; other
  unqualified refs are qualified with `src_name` when the sheets differ; already-qualified refs are
  left alone (except a qualified ref naming the source sheet that is inside the rect: shifted and
  re-qualified to dst, or unqualified if it now names its own home). Unchanged formulas keep their
  text byte-for-byte.
- Order: clears and `move_refs` run on the workbook, then the moved block is written, then the
  engine is rebuilt (`sheet_engine`) and recalculated.
- Tests: same-sheet B1→B10 gives `=A10*10`; the same cut moved to Sheet2!A10:B10 gives `=A10*10`;
  an outside `=C1` moved to Sheet2 gives `=Sheet1!C1`; H1 on Sheet1 becomes `=Sheet2!A10+Sheet2!B10`.

> 5. "A paste or cut that is refused must not end copy mode"

Conceded. `sheet_paste` returns whether it landed; Enter/cut spend the clip only on success.
Test: Enter-paste refused over part of an array leaves the clip live.

> 6. "Tiling a whole-column selection ... a freeze"

Conceded. Rule: an axis whose selection spans the whole sheet (all `MAX_ROWS` rows or all `MAX_COLS`
columns) is clamped to the larger of the sheet's used extent on that axis (from the selection's
start) and one tile, rounded up to a whole tile; the per-axis rule then applies to the clamped area.
After that, a tiled block over 100,000 cells (xlsxy's `MAX_PASTE_CELLS`) is refused with a status
and nothing is written. Tests: whole-column D with a 2x1 copy and used rows 1..6 tiles D1:D6; a
non-full selection that would make more than 100,000 cells is refused.

> 7. smaller points

- Shared formulas (`f_attrs` non-array) don't follow a move, as with insert/delete: added to Out of
  scope.
- Paste is keyed on `v.range()`'s top-left. After it, the selection is the pasted area with the
  active cell at its top-left (Excel): `sel` = top-left, `anchor` = bottom-right. Tests with the selection made top-down and bottom-up.
- AC5 extra tests: a filtered copy pasted at a single cell has the visible-row height; the clip's TSV
  skips filtered rows.
- Cross-tab cut: conceded, it pastes as a *translated* copy (one code path), keeps the source, and
  the status says the cut was kept as a copy. The clip is spent afterwards.

## Goal

In the desktop suite (`suite/docxy`), Ctrl+C / Ctrl+X / Ctrl+V / Enter on a sheet behave like Excel
for the five cases in the issue: a copy tiles over a paste area that is a multiple of it (and is
refused for a wrong-shaped one); pasted formulas have their relative references translated; Enter
pastes and ends copy mode; a cut only marks its source, and the paste *moves* the cells (source
cleared at paste time, references to the moved cells — inside and outside the block — follow them,
and the cut pastes once); a copy of a filtered range takes only the visible rows (rows hidden by a
filter), while rows hidden by Hide are copied.

## Acceptance criteria

Setup for 1–4 (the issue's): A1 1, A2 2, B1 `=A1*10`, B2 `=A2*10`, D1:D6 `old`, H1 `=A1+B1`.

1. Copy B1:B2, select D1:D6, paste → D1..D6 are `=C1*10` … `=C6*10` (two tiles of the 2×1 copy, each
   translated to its own corner); D7 untouched. Selection after the paste is D1:D6.
   - Copy B1:B2, select D1:D3 (3 is not a multiple of 2) → refused, nothing written, status says
     Excel's "The information cannot be pasted because the Copy area and the paste area aren't the
     same size and shape." (exact wording up to implementation; a test checks nothing changed).
   - Paste-area rule per axis: extent 1 → one copy's extent; extent a multiple of the copy's →
     that many tiles; else refused. A single-cell selection pastes the block once (today's case).
   - A tiled paste over part of an array is refused whole, as today (`refuses_paste` over the whole
     tiled block).
2. Copy B1:B2, select E1, paste → E1 `=D1*10`, E2 `=D2*10`. Single-cell copy translates too.
   `$` parts stay; a ref pushed off the grid becomes `#REF!` (existing `translate_formula`).
   Pasting a copy where it came from leaves the formula text byte-identical (no reprint).
3. Copy B1:B2, select F1, press Enter (not editing) → F1:F2 pasted (`=E1*10`, `=E2*10`), copy mode
   ends; a following Ctrl+V pastes nothing (nothing changes, not dirty) while the clipboard still
   holds that copy's text. If another app puts new text on the clipboard afterwards, Ctrl+V pastes
   it as today. Enter with no copy pending keeps its current behaviour (commit/move).
4. Select A1:B1, Ctrl+X → A1:B1 unchanged (not cleared, not dirty). Select A10, Ctrl+V → A10 1,
   B10 `=A10*10`, A1:B1 empty, H1 `=A10+B10` (value 11). One undo step restores everything
   (A1, B1, A10, B10, H1). A second Ctrl+V (at J1) pastes nothing. Ctrl+V pasting a *copy* twice
   still works (copy mode stays after Ctrl+V).
   - Move rule: every reference in the workbook (cell formulas on every sheet, array-formula
     `ref`s excluded — see Out of scope; defined names; CF/DV rule formulas) that points wholly
     inside the cut rectangle on the source sheet is shifted by the move offset (absolute parts
     too) and, if the destination sheet differs, re-qualified with the destination sheet. A range
     is moved only if both corners are inside the cut rectangle; otherwise it stays as is.
     The moved cells' own refs to cells *outside* the cut rect keep pointing where they did
     (qualified with the source sheet when moved to another sheet — xlsxy's precedent).
   - A cut pasted into a different workbook tab acts as a translated copy and keeps the source
     (status says so); Excel's cross-workbook move is out of scope.
5. With an AutoFilter hiding rows 3 and 5 of A1:B6, copy A1:B6 → the clip (and its TSV) holds
   rows 1,2,4,6 only; pasting at D1 writes D1:E4 and a formula from source row 4 landing on row 3
   is translated by its own row offset (−1 rows + col offset). A row hidden by Hide (not filtered)
   is copied. Cut is not affected (a cut moves the whole rect; it is not a filtered-row case).

## Approach

Pure, testable logic in `gridcore`; thin wiring in the suite.

- **`gridcore` (new `gridcore/src/edit/clip.rs`, re-exported from `edit`)**:
  - `paste_tiles(copy: (h, w), sel: (r0, c0, r1, c1)) -> Option<(u32, u32)>` — tiles down/across by
    the per-axis rule above; `None` = refused shape.
  - `translated_block(cells, src_rows: &[u32], src_col: u32, at: (u32, u32)) -> Vec<Vec<Cell>>` —
    each copied cell's formula translated by (target row − its own source row, target col − source
    col) with `translate_formula`; zero offset leaves the text untouched; an unparseable formula
    keeps its text. Build a whole tiled block from this (one call per tile, concatenated into one
    `Vec<Vec<Cell>>` for `paste_block_prechecked`, so one engine write and one array check).
  - `move_refs(wb, src_sheet, rect, dst_sheet, (dr, dc))` — the workbook-wide rewrite for a move,
    built on `rewrite_workbook_formulas`/`rewrite_if_changed` (unchanged text stays byte-identical)
    and a new `formula::move_ref_expr(e, home_sheet_name, src_name, rect, dst_name, dr, dc)` that
    follows the `adjust_for_edit` pattern (Ref, SpillRef, Range, Ref3D left alone, Col/RowRange left
    alone).
- **Suite (`suite/docxy/src/main.rs`)**:
  - `GridClip` gains: source identity (a per-`SheetView` id stamped at creation — add `clip_id: u64`
    from an app counter or an atomic), source sheet index, source column `c0`, the source row of
    each clip row (`Vec<u32>`, for filtered copies), and `cut: bool`. Plus an app field
    `grid_clip_spent: Option<String>`: the clipboard text of a copy that Enter-pasted or a cut that
    was pasted. While the clipboard still holds that text (`clip_still_ours`), Ctrl+V/Enter paste
    nothing.
  - `sheet_copy`: skips `row_filtered` rows (copy only), records the new fields; `cut` no longer
    calls `sheet_clear` (the array/protected refusals stay where they are).
  - `sheet_paste` (grid-clip branch): copy → tile check against the selection `v.range()`, build the
    translated tiled block, array refusal over the whole block, write, selection becomes the pasted
    area. Cut (same workbook) → like xlsxy's `paste_from`: clears of the source first (minus cells
    the paste overwrites, same sheet), refusal check over clears + block, then
    `edit::move_refs` across the workbook *before* writing the block (so the block's own internal
    refs are moved too — or equivalently move the block's formulas with `move_ref_expr` and the rest
    with `move_refs`; implementer picks one and tests H1 and B10), rebuild the engine
    (`sheet_engine`, as `structural_edit` does) and recalc; one undo step (`sheet_snapshot` is a
    whole-workbook snapshot). Then set `grid_clip_spent` and drop `grid_clip`.
    Cross-workbook cut → paste untranslated, keep source, status.
  - Enter (`"enter"` arm in the sheet key handler, ~main.rs:14571), when not editing and a grid clip
    is live (`clip_still_ours`): `sheet_paste`, then end copy mode (copy → `grid_clip_spent`).
  - TSV-text paste branch (external text) is unchanged, except it is skipped when the text is the
    spent clip's.
- Rejected: doing the tiling/translation inside `Engine::paste_block` — the engine's job there is
  arrays and spills; translation is a clipboard policy and xlsxy/gridwasm already translate before
  calling it. Rejected: clearing the cut source at Ctrl+X and "un-clearing" on Esc — Excel never
  changes the sheet on Ctrl+X, and undo would see two steps.

## Files

- `gridcore/src/edit.rs`, new `gridcore/src/edit/clip.rs` (+ tests there or `gridcore/src/edit/clip/tests.rs`)
- `gridcore/src/formula.rs` (`move_ref_expr`)
- `suite/docxy/src/main.rs` (GridClip, sheet_copy, sheet_paste, Enter arm, SheetView id)
- `suite/docxy/src/harness.rs` (required): `clipboard_app_json` reports `none` for a spent clip; its test gets that case
- `suite/docxy/src/sheet_entry_tests.rs` or a new `suite/docxy/src/sheet_clip_tests.rs` for suite-side tests
- `uiharness/cases/clipboard.uit` — new cases for the five scenarios (run on demand, desktop only)

## Tests

- gridcore unit tests: `paste_tiles` (1, multiples, refused, single-cell selection); `translated_block`
  (relative/absolute/mixed, zero offset byte-identical, per-row source rows); `move_ref_expr` and
  `move_refs` (H1 follows, inside-block ref follows, outside ref stays, partial range stays,
  cross-sheet qualification, defined name follows).
- suite tests that run the paste/cut logic against a workbook + engine without gpui (the pattern
  around main.rs:17890 `cut_d1_d3` etc.): if the gpui handlers cannot be called, factor the body of
  `sheet_paste`'s grid branch into a function taking `(&mut SheetView or &mut Workbook+Engine, clip,
  selection)` and test that. Each acceptance criterion 1–5 gets a test, including "second paste of a
  cut does nothing" and "one undo restores H1".
- harness cases in `uiharness/cases/clipboard.uit` for 1–5 (on-demand; implementer runs them if a
  desktop session is available and reports it either way).
- Commands:
  `cargo fmt --all`, `cargo clippy -p gridcore -- -D warnings`, `cargo test -p gridcore`,
  `cargo clippy --manifest-path suite/Cargo.toml -p docxy-suite -- -D warnings` (use the suite's real
  package name), `cargo test --manifest-path suite/Cargo.toml`, and `cargo test -p xlsxy` (shared
  gridcore code).

## Out of scope (follow-ups)

- Drawing the copy marquee (marching ants).
- Shared formulas (`f_attrs` non-array) following a cut, as insert/delete already leave them.
- Excel's `#REF!` for formulas that referenced the cells a cut overwrites.
- Cross-workbook cut-as-move.
- Moving array-formula `ref`s / CSE blocks, charts' refs and CF/DV *ranges* with a cut (formulas only).
- Same fixes in xlsxy (terminal) and gridwasm (web): tiling, Enter-paste, move refs, filtered rows.
- Paste Special, Office Clipboard etc. (#669 and the rest of batch #707).

## Open questions

None (cut with filtered rows: the whole rect moves, agreed).
