FIX r1 — all four findings verified against 94bf22c (full report: .workbench/review/revmux-r1.md)

## M1 (Major) — a cut pasted past the grid edge half-moves and turns refs to unmoved cells into #REF!
suite/docxy/src/main.rs `SheetView::paste_move` (~2985-3040). The block loop `break`s past
MAX_ROWS/MAX_COLS so those source cells stay, but `move_refs` and `move_block_formula` get the full
`clip.rect`, so refs to the cells left behind are shifted off-grid -> #REF!.
Example: A1 1, A2 2, H1 `=A2`; cut A1:A2, paste at A1048576 -> H1 `=#REF!`, A2 still 2.
Fix: refuse before anything changes (Refused(PASTE_SHAPE) or a clearer "can't paste past the edge"
message; keep the cut live) when r0+h > MAX_ROWS or c0+w > MAX_COLS. Excel refuses this paste.
Then drop the edge `break`s / the "isn't written, so its source stays" comment as dead.
Test: that example is refused, nothing changes, no undo step, the cut stays live and pastes at A10.

## M2 (Major) — copy mode survives edits, so a plain Enter later pastes the old clip
main.rs Enter arm (~14942) / `sheet_enter_paste`. Ctrl+C A1:A3; click C1, type 5, Enter (commits,
moves to C2); Enter again -> pastes over C2:C4 instead of moving. A stale cut instead swallows Enter
with "cut cancelled".
Fix (Excel: typing into a cell ends copy mode): record the view's `edit_gen` (and view id) on every
clip, copy or cut, and treat a clip as live for Enter-paste only while it is the same view and its
`edit_gen` is unchanged; an edit since spends it. Simplest consistent rule: `live` for Enter and
Ctrl+V both require no edit since in its own view (a copy pasted with Ctrl+V into another tab is
still live — the other view's edits don't count; and a Ctrl+V paste of a copy must not end its own
copy mode, so compare against the gen *after* the paste or re-stamp the clip after a Ctrl+V copy
paste). Pick the shape, but these must hold, each with a test:
- copy, commit an edit, Enter -> moves (no paste) and the clip is spent (Ctrl+V pastes nothing,
  harness reports `none`);
- copy, Ctrl+V, Ctrl+V again elsewhere -> both paste (copy mode stays after Ctrl+V);
- copy, Ctrl+V, Enter -> pastes again and ends copy mode;
- cut, edit, Enter -> moves normally (no swallowed key); Ctrl+V -> nothing, status cancelled is fine.
Note: if Ctrl+V-pasting a copy spends nothing but bumps edit_gen through its own undo step, the
re-stamp is required; test it.

## m1 (Minor) — doc/test comment about "a sheet protected since"
main.rs `paste_grid_clip` doc (~2877) and sheet_clip_tests.rs:381. Protecting after a cut takes an
undo step (`sheet_toggle_protection` -> `sheet_snapshot`) and so cancels the cut; the KeptAsCopy
branch is reached only for a cut taken off an already-protected sheet. Fix the doc to "off a
protected sheet" and the test comment/setup to a cut taken from a sheet protected at cut time.

## m2 (Minor) — a copy of only filtered-out rows makes an empty clip
main.rs `grid_clip` / `sheet_copy` / `paste_copy`. All selected rows filtered -> empty cells and an
empty TSV written to the OS clipboard (breaks `clip_still_ours`'s "Grid TSV is never empty"), and
paste returns Ok -> marks dirty, Enter spends.
Fix: `sheet_copy` with no visible row does not touch the clipboard or the clip (status says nothing
to copy, or just nothing); `paste_copy` of an empty clip is not "landed" (no dirty). Test both.

Re-run the Done commands (fmt, clippy -p gridcore and suite, cargo test -p gridcore, suite tests,
xlsxy), and clipboard.uit if you can. Reply FIXED <sha> with each finding marked fixed/disputed/deferred.
