# Review scope r2 — yeroo/docxy#664

Plan: `.workbench/plan.md` (v2; the "Changes from v1" section overrides the body where they differ).
Issue: `.workbench/issue.md`.

Diff: `git diff origin/main...HEAD` (fb170cf3 gridcore, 94bf22c3 suite, ca590abc fixes for round 1).

## What it is for
Desktop suite sheet clipboard behaves like Excel: copy tiles over a paste area that is a whole
number of copies (wrong shape refused), pasted formulas translate relative refs (per source row for
a filtered copy), Enter pastes and ends copy mode, Esc ends copy mode, Ctrl+X only marks and the
paste moves the cells (source cleared at paste, references to moved cells follow them across the
workbook, one undo step, pastes once, cancelled by any edit since the cut), filtered-out rows are
not copied (rows hidden by Hide are).

## Acceptance criteria
See plan.md "Acceptance criteria" 1-5 and the v2 changes (live-cut checks, Esc, harness `none` for
a spent clip, move option (b), refused paste keeps copy mode, whole-column clamp + 100k cap,
selection = pasted area with active cell at top-left).

## Look hardest at
- `gridcore/src/formula.rs` `move_ref_expr` / `move_block_expr`: qualification rules across sheets,
  ranges with mixed qualifiers, SpillRef, off-grid -> #REF!, byte-identical text when unchanged.
- `gridcore/src/edit/clip.rs` `paste_tiles` (whole-axis clamp), `tiled_block` (grid edge, short rows).
- `suite/docxy/src/main.rs` `SheetView::paste_move`: the order move_refs -> engine rebuild -> clears
  -> block -> late clears; overlap of source and destination on one sheet; refusal before any
  change; a single undo step; `cut_still_live` (could a legitimate cut be cancelled, or a stale one
  pass?); `edit_gen` bumps covering every edit path.
- `sheet_paste` / `sheet_enter_paste` / Esc arm: when the clip is spent, when copy mode survives,
  that the TSV branch still works for text from another app, dirty marking.
- Data loss: any path where a cut clears cells it should not, or a paste overwrites without undo.

## Out of scope
Marquee drawing; #REF! for refs to cells a cut overwrites; cross-workbook move; shared-formula,
chart, CF/DV range and array `ref` following a cut; xlsxy/gridwasm parity; Paste Special etc.

## Commands
`cargo test -p gridcore`, `cargo test --manifest-path suite/Cargo.toml` (package `docxy`).
Do NOT run interactive or GUI tests (uiharness needs a desktop session; do not run it).

## Round 1 fixes to verify (ca590abc; report: .workbench/review/revmux-r1.md)
- M1: a cut whose paste runs past the grid edge is refused before any change and stays live.
- M2: every clip records its view's edit_gen; an edit in its own workbook ends copy mode (Enter moves,
  Ctrl+V pastes nothing, harness reports none); a Ctrl+V of a copy re-stamps so copy mode survives
  its own paste. Check for a path where the restamp hides a real edit, or a paste that ends copy
  mode wrongly, and that other workbooks' edits don't count.
- m1: KeptAsCopy only for a cut off an already-protected sheet (doc/test).
- m2: a copy of only filtered-out rows copies nothing; an empty clip's paste is not "landed".
