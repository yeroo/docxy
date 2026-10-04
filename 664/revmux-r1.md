# Review: workbench / r1

scope: `C:\Users\boris\source\workbench\docxy-issue-664\.revmux\tasks\workbench\r1\input\scope.md`

## Major

### A cut pasted past the grid edge is half-moved: source cells left in place, yet every reference to them becomes #REF!

`suite/docxy/src/main.rs:2985-3040`

`paste_move` only checks that the paste area is a single cell or matches the cut's shape. It never refuses a destination where the block runs past MAX_ROWS or MAX_COLS. Its build loop `break`s for cells past the edge (around lines 2991/2997), so those cells are neither written nor cleared and stay where they were. The doc comment says the same: "a cell pushed off the grid's edge isn't written, so its source stays".

However, `move_refs(&mut self.pkg.workbook, src, &mv)` (line 3037) is given the full `clip.rect`, not limited to the cells that actually moved. `CellMove::holds` matches the unmoved cells, and `CellMove::shifted` turns their off-grid targets into #REF!. The same happens to the moved block's own internal references through `move_block_formula`.

Example: A1=1, A2=2, H1 `=A2`. Cut A1:A2, select A1048576 and paste. A1 moves to A1048576. A2 stays in place with its value, but H1 becomes `=#REF!`. Likewise, cutting A1:A3 and pasting at A1048575 turns `=SUM(A1:A3)` into `SUM(A1048575:#REF!)`. Excel refuses this paste. Here it lands as one undo step that corrupts formulas pointing at cells that still exist. No test covers a cut pasted at the grid edge.

Fix: In `paste_move`, before anything changes, refuse with `GridPasteError::Refused` (PASTE_SHAPE) and keep the cut when `r0 + h > MAX_ROWS || c0 + w > MAX_COLS`. The alternative is to narrow `mv.rect` to the cells actually written before calling `move_refs` and building the block formulas.

_confidence: 99 | sources: bugs+impl, docs+tests, adversarial | lenses: bugs, comments, tests, adversarial | verdict: confirmed_

### Copy mode survives cell edits, so a later plain Enter pastes the old clip over the selection

`suite/docxy/src/main.rs:14942`

Copy mode for a copy only ends on Esc (~14926), an Enter paste (~12386) or a pasted or cancelled cut (~12317). Typing into a cell and committing it does not end it, whereas in Excel, typing into a cell does. The new arm `"enter" if !editing && self.sheet_enter_paste(cx)` pastes whenever `grid_clip_live` is true, and `live` only checks `!spent` plus that the clipboard still holds the clip's text.

Sequence:
1. Ctrl+C on A1:A3.
2. Click C1, type 5, press Enter. You are editing, so this commits and moves the selection to C2.
3. Press Enter again to move on.

Step 3 pastes A1:A3 over C2:C4 instead of moving down, overwriting cells as an easily missed undoable edit. The plan's reasoning, "pasting a stale copy loses nothing", was about Ctrl+V. It does not hold once Enter, the ordinary navigation key, also pastes.

A stale cut causes the same problem. After any edit bumps `edit_gen`, the cut is still `live`. Enter then goes to `paste_move`, which returns `CutCancelled`, so the key is swallowed with a status message and the selection doesn't move.

Fix: End copy mode, or at least disarm Enter-paste, once a cell entry is committed or another undoable edit lands. For example, record `edit_gen` for copies as well and have `sheet_enter_paste` treat the clip as live only while `edit_gen` is unchanged. Alternatively, call `grid_clip_spend()` from the edit or commit path.

_confidence: 90 | sources: bugs+impl, adversarial | lenses: bugs, impl, adversarial | verdict: confirmed_

## Minor

### Doc says a cut "off a sheet protected since" becomes a copy, but protecting after a cut cancels it

`suite/docxy/src/main.rs:2877-2896`

The `paste_grid_clip` doc says: "A cut from another workbook, or off a sheet protected since, is pasted as a copy and keeps its source." The only UI path that protects a sheet is `sheet_toggle_protection`. It calls `sheet_snapshot()` ΓåÆ `push_undo` ΓåÆ `push_undo_snapshot`, which bumps `edit_gen`. Because `cut_still_live` is checked before `is_protected()`, protecting a sheet after a cut returns `CutCancelled`, not `KeptAsCopy`.

The `KeptAsCopy("...its sheet is protected")` branch is actually reached when the cut was taken from a sheet that was already protected at cut time. `sheet_copy` allows that, because `protected_refused` only checks Protected View and `sheet_clear_refused` only checks arrays.

The test comment at sheet_clip_tests.rs:381 has the same problem. It says "Protected without an undo step (as a direct path would)", but no such path exists, so the test sets up a state the UI cannot produce.

Fix: Change the doc to "or off a protected sheet" (protected when it was cut). Change the test comment so it describes a cut taken from an already-protected sheet.

_confidence: 85 | sources: docs+tests | lenses: docs, comments | verdict: confirmed_

### A copy of only filtered-out rows makes an empty clip that 'pastes' successfully and marks the workbook dirty

`suite/docxy/src/main.rs:2899-2911`

`grid_clip` now drops filtered rows on a copy. If every selected row is filter-hidden (for example, A3:A5 selected through the Name Box or Go To while rows 3-5 are filtered), both `cells` and the TSV are empty.

`paste_copy` returns `Ok(())` for h==0||w==0. `sheet_paste` therefore treats the paste as landed and calls `mark_sheet_dirty()` although nothing changed, and an Enter paste also spends the clip.

The Ctrl+C also writes "" to the OS clipboard, wiping whatever the user had there. The empty text breaks `clip_still_ours`'s stated invariant (line ~1381: "Grid TSV is never empty"). A "" clip counts as ours whenever the clipboard holds a non-text item, such as an image from another app, so the empty clip stays live.

Fix: In `sheet_copy`, refuse or skip creating a clip when the filtered row set is empty. Also have `paste_copy` return `Err(Refused(..))` or otherwise report "nothing pasted" for an empty clip, so nothing is marked dirty.

_confidence: 70 | sources: bugs+impl, adversarial | lenses: bugs, adversarial | verdict: confirmed_

## Sources

| agent | executor | model | effort | tokens | raised | status |
| --- | --- | --- | --- | --- | --- | --- |
| bugs+impl | claude | claude-opus-5-5 (requested opus) | high | 1945666 | 3 | ok |
| arch+quality | claude | claude-opus-5-5 (requested opus) | high | 1385704 | 3 | ok |
| docs+tests | claude | claude-opus-5-5 (requested opus) | high | 1086743 | 3 | ok |
| adversarial | claude | claude-opus-5-5 (requested opus) | high | 1724077 | 3 | ok |
