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

## Quote-led custom validation formulas (#2115)

- Only a `list` rule's formula that starts with `"` is inline items
  (`validation::inline_list(f, kind)`), never shifted. A custom `"Y"=A2` is a
  formula like any: `shift_formulas`, `lossless_shift`, `as_seen_from` and
  `RuleFormula::of` all decide by the rule's kind, so the dialog shows it
  written for the cell, a paste shifts its references, and `add_ranges` tells
  parts with equal text from different corners apart. The #1050 workaround
  in `cut_rules` (`quote_led`) is gone.

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

## An absent cell's inherited row or column style (#1660)

- In Excel, an absent cell under a row style (`<row s=.. customFormat="1">`)
  or a column style (`<col style=..>`) shows that style, so an explicit blank
  s0 cell there pins General (#1064, #1097).
  - `Sheet::inherited_style(r, c)` is the row's style (`row_style`), else the
    column's (`col_style`), else 0.
  - "Row over column" is an assumption, taken from how Excel materialises a
    new cell in a formatted row.
  - `pins_default` is `inherited_style != 0`. An `s` or `style` of 0, or one
    that cannot be read, gives 0.
- **Copies, fills and moves** read through `Sheet::copy_reader()`
  (`CopyReader`). A held cell is cloned. An absent cell inside the sheet's
  used extent (`used_size`, taken once) reads as a blank of its inherited
  style. Each row's and column's style is looked up once. Its users:
  - `ClipBlock::capture`;
  - `fill_changes`, and `autofill`'s absent Copy source (`absent_style`);
  - xlsxy's `copy` and gridwasm's `record_clip`, through `cell_opt`: `None`
    while the style is 0, and the TSV still comes from the real cell;
  - the suite's `cut_still_live` and `paste_move`. They must read exactly what
    `capture` stored, or every cut over such a cell is cancelled.
- **Past the used extent** (`past_extent`), an absent source reads as a
  default blank. Every writer lands it as the *destination's* absence,
  `CopyReader::absent_cell` on a reader of the destination's sheet. That is
  a blank of the destination's inherited style, which
  `set_cell` drops, so a held target ends absent and nothing is made where
  nothing was. A whole styled column copied elsewhere writes no million
  styled blanks, and a held cell in a styled column gets no s0 pin. The
  writers:
  - `fill_changes` and `autofill` (also FormatsOnly);
  - `paste_special_changes`, through `ClipBlock::used` and `lands_absent`.
    It never mixes a style for such a cell (No Borders and the Number
    Formats pastes would intern a new one). An absent destination gets
    nothing. An operation (Add and the rest) leaves a held destination's
    value and style alone: there is no operand. Otherwise, over a held
    destination:
    - All, No Borders, All and Column Widths, and Values and Source
      Formatting land the destination's absence.
    - Formats keeps the value with the inherited style.
    - Values, Formulas and both Number Formats pastes keep the cell's own
      style and clear its value, as an empty source inside the extent does.
      The source brings no number format.
  - the suite's tiled copy (`ClipBlock::land_absent`) and `paste_move`;
  - xlsxy's paste and gridwasm's `paste_own_clip`, through the extent kept
    in `ClipData` or `GridClip`. xlsxy's Paste Special sets `ClipBlock::used`
    from it.
  - This differs from Excel, which would carry the source's format there:
    a whole-column copy in Excel copies the `<col>` style itself, and that
    is out of scope.
- **Paste Special's own destination** `d` is read through the destination's
  reader: the held cell, or `absent_cell`, with no extent bound. So Values
  keeps an absent cell's inherited format.
- **Entries** are read under `Sheet::entry_style_at`: the cell's own style,
  or an absent cell's inherited one. Its users:
  - typing (`entry_cell_ctx`, `typed_formula`, so Ctrl+Enter too);
  - text pasted from outside: xlsxy, gridwasm, and the suite's `fields_block`;
  - AutoSum, in the suite and xlsxy;
  - xlsxy's data form, for a record's own field. A new record reads the field
    above it, or the new cell's inherited style when that field is absent.
  - So `15` in a `0%` column is 15%, and a Text column keeps `=1+1` as text.
- **`Sheet::set_cell`** never stores a blank of exactly the slot's non-zero
  inherited style, whether or not the slot holds a cell. Such a blank is the
  absence written out.
  - Blank s0 keeps the #1097 rule: it is dropped over an absent slot and kept
    over a present pinned one, so Del keeps a file pin.
  - The loader inserts cells directly, so a file's blank `s=` cells
    round-trip as they were.
- **Cuts** clear their source to its inherited style, so it ends absent: the
  suite's `paste_move` and xlsxy's paste. xlsxy's cut also blanks the
  target cells it covers past its used cells to the target's inherited
  style. gridwasm's cut clears contents only, as Del does.
- **Spill output**: a dynamic array's or legacy CSE block's absent member is
  made with its inherited style (`engine::spill_slot`), as Excel saves spill
  output in a formatted column. A member's own style, and the anchor, are
  kept. So a spill shrunk and grown again in a `0%` column stays `0%`.
  - `snapshot_cells` records a live spill member of style 0 (a file's, from
    before) as `None`.
  - A member of its inherited style is `Some(blank)`, which `set_cell` drops
    on restore.
  - Either way, the undo of a value typed over a member leaves no pin, under
    manual calculation too.

## A cut paste's formulas: local names, bare table refs, rules left in place (#1687)

- A moved cell's own formula goes through `gridcore::edit::moved_cell_formula`
  in both hosts (xlsxy `paste_from`, the suite's `paste_move`), read on the
  workbook as it was before the cut. On top of `move_block_expr`:
  - Moved to another sheet, a bare name with a definition in the source
    sheet's own scope is qualified with it (`Rate` → `Sheet2!Rate`), and so is
    a called local lambda (`MyFn(3)` → `Sheet2!MyFn(3)`, an `Expr::Call` on
    the scoped name). A workbook name, a builtin and a LET/LAMBDA binding stay
    bare (`formula::qualify_local_names`).
  - Moved out of the table it sat in (to another sheet, outside it, or into
    another table), its bare `[@Col]` / `[Col]` name that table
    (`Table1[@Col]`). A shared formula held verbatim gets neither.
- `Workbook::defined_name` resolves `Sheet2!Rate` to Sheet2's own definition
  from any sheet, and to nothing when Sheet2 has none (still unsupported and
  frozen, #1632). Every engine and evaluator name lookup goes through it;
  `INDIRECT("Sheet2!Rate")` stays `#REF!`. A definition's body still reads
  against the formula's sheet.
- Not handled: a bare name whose source sheet has no local definition but the
  destination has one switches to the destination's (Excel would write the
  workbook form, `Book1.xlsx!Rate`, which the parser does not take); a `.xls`
  save freezes a `Sheet2!Rate` cell to its value with a report.
- `move_refs_rules_stay` (both hosts) leaves a conditional-format rule whose
  ranges meet the cut in place with its relative and mixed references, but
  re-points its fully absolute ones (`$F$1`, `$F$1#`, an all-`$` range with
  both corners moved) to moved cells outside its own ranges, which name
  those cells themselves. One into its own ranges reads the rule's own
  cells, which stay with it (`A1>AVERAGE($A$1:$A$10)` on A1:A10, #880), so
  it stays (`formula::move_abs_ref_expr`). `FormulaSite::rule` carries the
  rule's ranges to the per-formula rewrite.
- xlsxy asks `cut_moves_refs` (the rewrite's dry run) before writing: only a
  cut that re-points something outside its own cells takes the
  whole-workbook snapshot and becomes a `Structural` step; the rest stay a
  `Cells` step. The dry run and the rewrite are one walk
  (`walk_workbook_formulas` with `write` off), so they cannot disagree; it
  takes the workbook mutably for that and writes nothing.
- A restyle (`apply_styles_on`, xlsxy's only style-only writer) of a cell a
  pending cut was taken from ends the cut, as an edit does (`end_cut_on`): the
  paste is then a copy of the cut-time cells, and the source keeps its format.

## `.xls` page setup, page breaks and outlines (#1440)

- `gridcore/src/legacy/page.rs` holds the BIFF8 page setup codec that both
  the `.xls` reader (`xls.rs`) and the writer (`xls_write.rs`) use: SETUP,
  the four margins, HEADER/FOOTER, HEADERFOOTER, HCENTER/VCENTER,
  PRINTHEADERS/PRINTGRIDLINES, WSBOOL, HORIZONTAL/VERTICALPAGEBREAKS, GUTS.
  ROW and COLINFO carry the outline level and collapsed flag.
- A sheet substream starts from BIFF's absent defaults (`page::absent`):
  margins 0.75/0.75/1/1 and header/footer 0.5, not the model's Normal
  ones. So the writer always writes all four margins and SETUP.
- fNoPls makes only paper, scale, resolution, copies and orientation
  undefined. Header/footer margins, fit counts, the first page and the
  print options count either way: oracle-basic has fNoPls with header 0.5,
  as its `.xlsx` has. Orientation is three-state: fNoPls or fNoOrient is
  `Default`. The writer always clears fNoPls (600 dpi, one copy) and writes
  `Default` as fNoOrient.
- Mask HEADERFOOTER's flags with 0x000F: Excel leaves reserved high bits
  set (0x60CC in the corpus). Excel writes it right after SETUP, before
  DEFCOLWIDTH, and the writer does the same
  (`xls_page_setup_records_come_in_excels_order`).
- A custom view's (USERSVIEWBEGIN..END) page setup records are skipped: they
  are the view's, and custom views are named.
- Breaks: every BIFF8 break is manual. The reader keeps
  ` [min="s"] max="e" man="1"`, and the writer writes only `is_manual()`
  breaks over their min/max span, cut to the grid. Automatic ones are
  dropped without a name.
- `build_counted` copies the page setup, breaks and outline settings into
  the `Sheet` and leaves `page_setup_loaded`/`outline_loaded` at the fresh
  part's defaults, so Convert (a save as `.xlsx`) writes them.
- What BIFF8 can't hold is named with the feature's own words: "page setup"
  for a header or footer over 255 characters, a first page over 32,767 or a
  paper code over 65,535; "page breaks" past the grid or past 1,026;
  "outlines (grouped rows and columns)" past the grid or deeper than 7.
- The corpus has no outlines or breaks, so these rest on synthetic round
  trips. An Excel-made `.xls` still asks on Ctrl+S because its document
  properties are dropped at import (a later slice of #1440).

## Formats, widths, merges, comments, validation and conditional formats from `.xls`/`.xlsb`/`.ods` (#884)

- The three importers (`gridcore/src/legacy/`) now keep what the model
  holds of each: a cell's bold, italic, font colour, size and name, solid
  fill and thin box; column widths; merges; comments (author and plain
  text); data validation; and `cellIs`/`expression` conditional formats with
  their differential format. Alignment, wrap, row heights and charts are
  still parked. `.xls` names what it leaves out in `dropped`; `.xlsb` and
  `.ods` have no such list.
- `SheetIn` carries `comments`, `validations` and `cond_formats` (`CfIn`).
  `BookIn::build` writes comments with `set_comments`, appends validations
  as rules built in memory (`ix`/`orig` none, which a save appends), and
  writes conditional formats through `SheetPackage::add_cond_formats`. A
  block with `ix: None` is never written by `set_cond_formats`, so the
  import can't leave them in the model alone. `add_cond_formats` is the
  batched writer `add_conditional_format` now calls: dxfs once, all of a
  sheet's blocks in one splice.
- Rule formulas are in Excel's syntax, relative to the model's anchor: the
  least row and least column of the rule's ranges, as
  `validation::anchor_of` and `cf::cell_dxf` take it (`legacy::anchor_of`).
  That cell is not always the first range's top-left (`D5:D10 B2:B3` is
  anchored at B2). Each reader decodes against the base its format defines
  and records it (`DvIn::base`, `CfIn::base`), and `BookIn::build` moves the
  formulas from there to the anchor (`DvIn::placed`, `CfIn::placed`, with
  `translate_formula`). A rule whose formula must move and doesn't parse,
  or whose move pushes a reference off the grid (a new `#REF!`), is left
  out, and `.xls` names it. Then `fix` runs (sheet renames, `file_formula`). The bases:
  - BIFF8 CF: its CONDFMT's refBound top-left;
  - BIFF12 CF: the ranges' bounds' top-left;
  - BIFF8 DV and BIFF12 BrtDVal: the first range's top-left, as LibreOffice
    reads them;
  - `.ods`: each condition's own `base-cell-address`. The `.ods` reader moves
    each CF rule itself, since a block's rules have bases of their own.

  BIFF8/BIFF12 formulas decompile with `Base::Shared(base)`, which reads
  ptgRefN as offsets and ptgRef as absolute coordinates.
- `.xlsb`: `xlsb_style.rs` reads `styles.bin` (BrtFont, BrtFill, BrtBorder,
  whole BrtXF, BrtDXF's XFProps) into model formats, made once per XF.
  Excel's default text colour (theme 1, no tint) is no colour, as the
  `.xlsx` loader reads it. The other theme colours are `*_unresolved`.
  Indexed colours use the default palette.
- `.ods`: cell styles resolve through their parents, `Default` and the
  `table-cell` default style. A value in no style of its own takes the
  document default's font, as the `.xls` reader keeps a workbook's Arial 10
  (Excel's `.xlsx` of an `.ods` whose default is Aptos Narrow has it on
  every cell). An empty cell is kept only when its format differs from that
  default (so LibreOffice's Liberation Sans 10 keeps no blank), and never
  in a run that reaches the grid's last row or column (padding). Styled
  blanks have their own cap, `Limits::styled_blanks` (1,000,000). Past it
  they are left out, never refused, and they never count against the
  repeat budget, so no file that opened before is refused now.
- An `.ods` empty cell with no style of its own takes its column's default
  style, so its run walks the column styles it crosses. The walk happens
  only when some column default style gives a blank something to keep (a
  format, or `style:map`s). Adjacent columns in one style are one step.
  The total is capped by `Limits::column_walk` (4,000,000 steps); past it,
  further blanks take no column style, never an error.
- `.ods` column widths: the width most columns have is the sheet's default
  (no definition; `default_col_width` unless it is Excel's own). Only the
  others become definitions, `paginate::width_of_px` of the length at 96
  dpi (the inverse of `width_px`). This matches Excel's `.ods` (1.5875cm =
  60 px for 8.54 characters). LibreOffice's own widths are unverified.
- `.ods` repeats: a span is a merge in every copy of a repeated row,
  charged as repeats. An annotation is on the first copy only. Padding
  rows make neither. Validation and conditional-format ranges are row runs
  that grow into rectangles, so a repeat adds one range. A table with
  LibreOffice's `calcext:conditional-format`s takes those and ignores its
  `style:map`s, which are their copies.
- `.xls` notes: an OBJ whose FtCmo has ot 0x19 is a note; the TXO after it
  holds the text (cchText at offset 10, the text in its CONTINUE pieces).
  Only another OBJ, or a chart substream, names "pictures, shapes and
  charts"; MSODRAWING alone doesn't. CF12, CONDFMT12 and CFEX name
  "conditional formatting".
- `.xls` CF records are read after the sheet's cells (the cell closure
  borrows the styles). A CF's solid fill is its DXFPat background colour,
  as an `.xlsx` dxf's `<bgColor>`.
- The xlsx loader never sets `Xf::border`: a loaded thin box lives in the
  `<xf>`'s `<border>` through `loaded_from`. The round-trip tests check it
  with `legacy::saved_thin_box`.
- The legacy oracle (`gridcore/tests/legacy.rs`) checks (i): the cells'
  fonts and fills, column widths and merges against the `.xlsx` source,
  with Calibri 11 as the default on both sides. Not the thin box: the
  source's `Xf::border` is always false (above), so the readers' round
  trips check it with `saved_thin_box`. The corpus has no CF, DV or
  comments, and no Excel-made fixture has them, so those rest on
  hand-built records laid out from [MS-XLS]/[MS-XLSB]/ODF.

## References into other workbooks from `.xls`/`.xlsb` (#903)

- Every external book the import can name becomes an `ExternalLink`, `[k]` in
  file order. In an `.xls`, that is a SUPBOOK whose virtPath decodes
  (`xls::virt_path`). In an `.xlsb`, it is a BrtSupBookSrc whose part is read
  (`sbt` 0, its rel there). A book needs no function name any more (it did in
  #888/#890), as Excel keeps every book. Each link counts against
  `Limits::links`. Its cached cells count against `Limits::cells`, charged
  one by one as they are read (CRN, BrtExternCell*), so a crafted cache is
  refused before it outgrows the budget.
- virtPath forms with an Excel oracle are decoded:
  - `\x01\x08file`: Library, `xlLibrary` rel.
  - `\x01file`: beside the workbook, target `file`, as ext-name.xls has it.
  - `\x01\x01C\x03dir\x03file`: `file:///C:\dir\file`, with a space as `%20`
    (the spelling of the `.xlsb`'s absolute rel).

  A target is a URI reference, so in the last two forms `%` is written `%25`
  and `#` is written `%23`. The Library form stays raw: the `.xls` writer
  reads it back as a plain file name.

  The other forms (same volume, `\x03`/`\x04` relative directories, long or
  UNC volumes, startup directories, unencoded, DDE/OLE) give no link, so a
  formula referring to such a book is dropped and its value kept, as before.
- References: ptgRef3d/ptgArea3d through an XTI naming one sheet of a linked
  book give `[1]P!A1`. When the sheet's name needs quotes, the book number goes
  inside them: `'[1]My Sheet'!A1` (`legacy::external_prefix`).
  ptgRefErr3d/ptgAreaErr3d give `[1]P!#REF!`. A span over the book's sheets
  and a sheet the book doesn't list stay unread.
- Names: `[1]!Prices`. Each name keeps its `refersTo` from
  `legacy::ext_definition`:
  - a ptgErr gives `#REF!`;
  - the 13-byte area token gives `='P'!$A$1:$A$2`;
  - the 9-byte cell token gives `='P'!$A$1`.

  The sheet is always quoted, as Excel quotes it. The token is the ptg,
  itabFirst/itabLast u16, then BIFF8's RgceArea/RgceLoc, in both formats; the
  only evidence is the oracle's bytes. Any other shape, a span or a relative
  part gives no `refersTo`.
- Name scope: an `.xls` name's `ixals` becomes `sheetId`, and the writer dedupes
  names by (name, scope). A formula through the whole book names only a
  workbook-scoped one. The `.xlsb` scope (BrtSupNameBits) isn't read.
- Cached cells: the `.xls` XCT/CRN (SerAr) records and the `.xlsb`
  BrtExternTable/Row/Cell records become `<sheetDataSet>`. A number has no `t`,
  text is `str`, a boolean `b` and an error `e`. Blanks are left out.
- Neither `[1]P!A1` nor `[1]!Name` parses, so the engine keeps the cached
  value. A quoted `'[1]My Sheet'!A1` does parse: `Eval::resolve_sheet` marks a
  missing sheet whose name holds `[` unsupported, so it keeps its cached value
  too, for an `.xlsx` as well. This doesn't apply to a reference that
  INDIRECT's text names (`Eval::indirect` depth): that one is `#REF!`, as in
  Excel for a closed book. Marking it would freeze the cell after the text
  changes. Because these don't parse,
  `BookIn::build` can't carry a sheet rename into them. One that names a
  renamed sheet is dropped (`names_external_book`).
- Not done:
  - the `.xls` writer writes only a Library book's SUPBOOK, so Save back to
    `.xls` (xlsxy's Compatibility Mode) loses these formulas, after naming
    the loss;
  - the parser reading `[k]` references;
  - sheet-scoped external names through a sheet XTI;
  - 3D spans across another book.
