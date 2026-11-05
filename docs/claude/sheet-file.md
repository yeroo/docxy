# Sheet file model

Notes moved from `CLAUDE.md` (#1903). Read them before changing a sheet's notes, slicers, data validation or hyperlinks in `gridcore`, and the dialogs and Undo over them. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Sheet notes follow row/column insert and delete (#1335)

- `SheetPackage::shift_comments(sheet, &EditShift)` (gridcore
  `comments.rs`) moves a sheet's notes, threaded comments and their `tc=`
  shadows with a whole-row/column insert (`delta > 0`) or delete (`< 0`); those
  in a deleted span are dropped (an insert that would push one off the sheet is
  refused like one for cells: `insert_shifts_comments_off`), and nothing is
  written when none moves. `SheetView::structural_edit` calls it, and takes a
  package snapshot (not the workbook one) only when the sheet has comments.
  It re-serializes the sheet's notes, so rich-text runs inside a note flatten,
  as on any other note edit. xlsxy (terminal), Insert/Delete Cells and Sort do
  not move notes yet.

## Table slicers on save, and Undo of a table step (#1694)

- `gridcore` has no slicer model. When a save drops a `tableColumn` or a whole
  table part, `drop_table_slicers` (`xlsx.rs`) removes the table slicer caches
  (`x15:tableSlicerCache` naming that table `id` and column id) with their
  workbook rel, `x14:slicerCache` entry and `Slicer_<name>` defined name, and
  the slicers on them (their element in the slicer part, the part with its
  sheet rel and `x14:slicer` entry when none is left, their drawing anchor).
  Other slicers keep their bytes. Dropping rather than keeping is unverified
  against Excel's repair. A sheet removal records the ids of the table parts
  it drops (`SheetPackage::removed_table_ids`), and the save drops their
  slicer caches too (#1741); ids, not a "names no part" test, as a table
  added later takes the first free part and its id.
- `App::restore` (xlsxy) folds the renames between the current and the restored
  table state over the data model's measures and relationships
  (`model_edits_for`), so ones added after a table step follow its Undo/Redo;
  the step's recorded `model_edits` still restore its `#REF!` cases.

## Data Validation dialog shows x14 rules (#1817)

- The loader reads an `<x14:dataValidation>`'s attributes as a classic
  `<dataValidation>`'s (`read_dv_attrs`, one reader for both) into
  `ExtRule::validation` (`ExtValidation`: the attributes, and which of
  `ExtRule::formulas` are formula1 and formula2, so every edit that moves the
  formulas keeps the rule current); `ExtRule::to_validation` builds it.
  `validation::dialog_rule_at` is what both hosts' dialogs open on and pass
  as `current`: the classic rule covering the cell, else the first
  meaningful x14 one. `validation_at` stays classic-only, so entry checking,
  dropdowns, input prompts and circles still ignore x14 rules (follow-up).
- `set_validation` with an x14 rule shown: OK of what the untouched dialog
  makes of it (`DialogBoxes::of(..).rule(..)`, so an `operator` Excel leaves
  out compares as `between`), over cells that all carry an x14 rule like it
  and no classic rule, changes nothing, Apply to all or not (the suite and
  xlsxy say "Data validation unchanged", no undo step), so the element keeps
  its bytes. "Any value" strikes the x14 rule shown (with Apply to all those
  like it, `cut_ext_rules_where`); one the dialog did not show stays (#1734).
  Any other rule replaces them all with a classic `<dataValidation>` (an
  other-sheet list included), losing attributes the model does not know
  (`imeMode`); unmeasured against Excel. Clear All's Apply to all reaches
  like x14 rules too. No x14 rule is ever written.

## Pivot slicer and timeline caches on save (#1797)

- `remove_pivot` notes the pivot as `(sheetId, name)` in
  `SheetPackage::removed_pivots` (sheetId of its sheet's `<sheet>`, name from
  its table part; either not found means nothing is noted), and `remove_sheet`
  goes through it. The save runs `drop_pivot_slicers` right after
  `drop_table_slicers`: a cache in `xl/slicerCaches` or `xl/timelineCaches`
  whose `<pivotTables><pivotTable tabId name/>` entries are all noted goes with
  its slicers or timelines (`SlicerKind`: `SLICERS`, `TIMELINES`; part, sheet
  and workbook ext entries, `tle:timeslicer` anchors, relationship, defined
  name), through the `drop_slicer_caches` shared with tables; a cache that
  also filters a pivot that stays only loses the noted entries. A pivot added later under a
  noted pair keeps it noted (its cache is its own). `remove_pivot` keeps the
  cache part, override, workbook rel and `<pivotCache>` entry while another
  pivot shares the cache. Synthetic fixtures only, as for
  table slicers; restoring slicers on a sheet restore is not done.
- The `Slicer_<name>` name of a dropped slicer cache stays in
  `Workbook::defined_names` until a reload (the file is right on every save).
  The suite's Go To leaves `Slicer_` and `NativeTimeline_` names whose formula
  is `#N/A` out of its list (a user's own name over a range stays).

## Sheet hyperlinks in .xls, and file URI forms (#1726)

- `gridcore/src/legacy/hlink.rs` is the HLINK record both ways: `body` for
  `xls_write` (after MERGEDCELLS), `parse` for the `.xls` reader. A model target
  is written as Excel writes it: `#loc` a location string (never split), a UNC
  path a string moniker (`hlstmfMonikerSavedAsStr`), a target with a scheme
  (2+ letters, so `C:` is a drive) a URL moniker, any other path a File moniker
  holding the path as the model has it (native text: `%` is literal; relative:
  leading `..\` in `cAnti`, at most 255, no `hlstmfIsAbsolute`; a non-ASCII
  path adds the Unicode extension). A path's `#loc` (a cell of another
  workbook: the first `#` whose tail is a reference with `!` or a defined name,
  no `\` or `/`) is that moniker plus a location, as the reader reads it
  (`a#b.xlsx` is still split: the model holds one string). No display name,
  frame or ScreenTip (#1454).
- `LinkRef::target` is a range's own target. A `hyperlink_refs` range is
  written whole, by both writers, only while every cell it spreads over (the
  anchor alone for a range over 4,096 cells) carries that target; otherwise its
  cells go one by one, so relinking a whole column's anchor writes only the
  anchor. The `.xls` writer clips a range to the grid (a whole row then reads
  back spread over its 256 cells) and writes the other link cells after the
  ranges, so a cell's own link in a range wins on read. Overlapping ranges go
  in anchor order, not the file's, so which one an overlapped cell takes in
  Excel after an `.xls` round trip can change (a known limit: an anchor-only
  range does not hold its other cells). Past the grid or over `MAX_BODY` is
  counted in `lost`.
- The reader skips a record it can't read (named "hyperlinks"; `cAnti` over
  255 is one), takes a moniker plus location as `url#loc`, spreads ranges
  through `hyperlink_cells` (charged as repeats, `charge_repeats`) and keeps
  each multi-cell one in `hyperlink_refs` (`loaded == now`), so edits move it
  and `write_hyperlinks` writes a ref whose `loaded` names no element of the
  block as a new `<hyperlink>` over `now` (in `hyperlinks_removed` it strikes
  nothing). HLinkTooltip (0x0800) is named "hyperlink ScreenTips". Checked
  against xlrd only (no Excel oracle).
- The model holds paths natively, UNC and scheme-less alike (`has_scheme`, in
  `xlsx/hyperlinks.rs`, is the one scheme test; `C:` is a drive): an xlsx
  `Target` (a URI reference) is decoded on load and `%`-escaped on write.
  `xlsx::model_target` maps every `file:` spelling of a UNC path
  (`file:///\\s\sh`, `file://s/sh`, `file:////s/sh`, `file://///s/sh`, escapes
  decoded in each) to `\\s\sh`, and decodes a scheme-less path's escapes
  (`My%20Book.xlsx`); `localhost`, a drive or port in the server's place
  (`file://C:/x`, `file://h:80/x`), local `file:` paths, a `file:` URI with
  `?`/`#`, URLs, locations, bad escapes, and an escaped separator, `#` or `?`
  (`%2F`, `%5C`, `%23`, `%3F`, which would change what the path names) stay
  raw (Excel's `file:///\\…` spelling then keeps its UNC path, undecoded). `wire_target` writes
  a UNC path as `file:///\\…` and any native path with `%` escaped as `%25`, so
  it reloads as written. The save compares model values through `canonical`
  (`model_target(wire_target(m))`, never `model_target(m)`, which would decode
  a native `%`) and reuses relationships through it, so a raw spelling held in
  the model is no change and no new relationship. An `.xls` File moniker's path
  is native and read as it is; URL and string monikers go through
  `model_target`.
