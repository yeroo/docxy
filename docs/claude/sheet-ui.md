# Sheet UI

Notes moved from `CLAUDE.md` (#1903). Read them before changing the sheet's tab rename, Font boxes and Save As names. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Sheet tab rename (#1365)

- `SheetRename` (`idx`, `text`, `caret`, `sel`) is the inline rename's state,
  edited through `field_edit` like the Name Box: it opens with the whole name
  selected, Left/Right/Home/End/Delete/Backspace act at the caret, Ctrl+A/C/X/V
  copy and paste one line (Ctrl+Alt is text, any other chord types nothing).
  Enter and the exit path still commit `text`; Escape keeps the old name.
  `field-read sheet-tab-rename` reports the caret and `[0,len]` selection.
  Click-to-place and Shift+arrows are not there.

## Sheet names Excel refuses on rename and add (#2115, #2153)

- `gridcore::sheet::sheet_name_error(name)` is the rule, with the reason a
  host shows: empty, over `SHEET_NAME_MAX` (31) characters, any of
  `SHEET_NAME_FORBIDDEN` (`[ ] : * ? / \`), a control character
  (`sheet_name_control`: `char::is_control`, tab and line breaks too, and
  U+FFFE/U+FFFF; the save drops what XML 1.0 forbids, so `Data\u{1}` would
  save as a second `Data`, #2153), or an apostrophe first or last (`Bob's
  Data` is fine). `legacy::valid_sheet_names` cleans imported names by the
  same rules (`_` for a forbidden or control character).
- `Workbook::sheet_name_refused(name, except)` adds the taken check
  (`SHEET_NAME_TAKEN`): another sheet's name folded with `to_lowercase()`,
  not ASCII only, as Excel compares (`Données` and `DONNÉES` are one name,
  and a file holding both is repaired). `except` is the sheet a rename
  renames, so a case-only rename holds. Every add or rename of an outside
  name goes through it (#2153), xlcomshim's `Worksheet.Name` put too (a
  refused name is `E_FAIL`); `sheet_names_match` is the same fold for two
  names, which gridwasm's `sheet.restore-removed` uses between stashes. `Workbook::sheet_index` and
  `next_sheet_name` still fold ASCII only (formula resolution).
- `SheetPackage::add_sheet` takes only a name that passes it: a host checks
  an outside name first and says why. Debug builds assert it, so a path that
  skips the check fails its tests; code-chosen names (`SheetN`, `PivotN`)
  pick a free one themselves.
- Hosts that add a sheet from an outside name: xlsxy's AddSheet prompt (the
  reason on the status line), `sheet.add` and `sheet.import-csv`/`-text`
  (`"<verb>: <reason>"` for a name Excel refuses; a taken one is deduplicated
  by `Workbook::free_sheet_name`, `Name 2`… cut to fit 31 characters, the
  verbs' documented contract), and `pivot.create` in both xlsxy and gridwasm
  (`"pivot.create: <reason>"`, or its documented `sheet name '<name>' is
  already taken`).
- Opening a `.csv`/`.tsv`/`.prn` in xlsxy names the sheet after the file
  stem through `legacy::clean_sheet_name` (the import's cleaning: `_` for a
  forbidden character, cut to 31, apostrophes trimmed off its ends), since a
  `.tsv` is rebound to an `.xlsx` that would otherwise hold `a[1]`.
- gridwasm (the VS Code tab): `sheet\tadd` and `sheet\trename` trim the
  webview's typed text and check it; a refused one sets `err` (the cell-ref
  tooltip) and pushes no step and no `edits` bump. `sheet\tadd` with no name
  (the "+" button) adds `next_sheet_name()`. The ctl `sheet.rename` checks
  first and errors with the reason.
- `XlsxPackage::rename_sheet` declines a name `sheet_name_refused` refuses (so `close::commit_rename_buffer`
  and its exit path change nothing). The suite's Enter on the rename bar with a
  changed, refused name sets the status line to the reason and closes the bar;
  Excel keeps the bar open behind a message box (a known deviation).
- xlsxy's `App::rename_sheet` returns the reason as `Err`, which the Rename
  sheet prompt and the `sheet.rename` control show.

## Fixed-denominator fractions and fractional seconds (#2115)

- `gridcore::numfmt`: a digit 1-9 right after a bare `/` starts a fixed
  denominator (`Tok::FixedDen`): `# ?/4`, `# ??/16`, `?/8`, `# ??/100`. It
  renders the nearest numerator over that denominator, unreduced (`2/4`),
  carrying to a whole (blank fraction as wide as `?/4`). A digit anywhere else,
  or a fixed denominator outside a fraction shape (`d/4`), refuses the code.
- In a date section, `.0`/`.00`/`.000` right after `ss` or `[s]` fold into
  `Tok::SubSecond` (`mm:ss.0` is built-in 47). `clock_of` rounds once to the
  shown precision: a clock time within its day, clamped at its last unit
  (23:59:59.9) as `serial_to_parts` clamps whole seconds; a section with
  elapsed time (`[h]`, `[m]`, `[s]`) rounds the whole span and takes every
  part from that total, so `[m]:ss` at 59.6 s is `1:00`.
- Format Cells does not list `mm:ss.0` or Excel's fixed-denominator types yet
  (a follow-up); such a cell opens as Custom.

## Sheet Home ribbon Font / Font Size boxes (#1344)

- The two boxes are text fields: `Docxy::font_box` (`FontBoxEdit`, tied to the
  tab and sheet like the Name Box) opened by a click on `sheet_combo`, keyed by
  `font_box_key`; Enter applies the name (`Font` undo step) or a size of 1 to
  409 (`Font Size`; a bad one keeps the box and says so), Escape or a press
  elsewhere restores, and a protected sheet refuses Enter (Protected View and a final document hide
  the ribbon body, so the box cannot open there). `sheet_field_live`
  (Name Box or font box) is what the key gates ask; the box is live only while
  the Home tab's body is drawn (a tab switch or Esc on its flyout closes it). Unfocused they show the
  active cell's font and size (`font_box.rs`). `uiharness/cases/sheet-font-box.uit`
  covers what `sheet-inputs-typing.uit` does not.

## Sheet Save As names (#804, #1807)

- A workbook Save As refuses, as Excel does, a file name holding
  `< > ? [ ] : | * "` (the brackets are external-reference syntax) or a full
  path over 218 UTF-16 units, the added extension counted and not
  canonicalised (`gridcore::save_name::check`, `BAD_NAME`, `MAX_PATH`,
  shared by the suite and terminal `xlsxy`; xlcomshim's `Workbook.SaveAs`
  does not check). The suite (`sheet_save_as_name`): the harness's
  `save-as` (before "already exists"), `save_sheet_to`, and the Save As
  dialog's answer, which sets the status and writes nothing like a failed
  write. Terminal `xlsxy` (`App::refuse_bad_save_name`, after Protected
  View's refusal in `request_save_as` and `save_as_type`): Backstage Save As
  (`save_name::typed_name_ok` first checks what the join to the folder
  would hide: the typed name's last piece after `/` or `\`, and a Windows
  drive-relative `c:report`, which would replace the folder; a folder part
  like `[old]/book` is fine) and the F12 prompt (a typed path is checked as typed,
  so `c:` there is a drive), for every Save As
  type (text, web page, PDF, `.xls`), before the macro-free and
  compatibility questions and before the binding moves. A folder may hold
  the characters. In-place Save never asks, so a workbook opened from such a
  name saves over itself. Documents and Projects are not checked.

## The cell editor's commit and sheet protection (#1054, #2230)

- No editor opens on a locked cell of a protected sheet (`sheet_begin_edit`,
  the typing gate, the pick lists, Error Checking's Edit), and the commit is
  the backstop: `commit_edit_taken` refuses an entry whose cell (the
  editor's origin, `edit_origin`) is locked on a protected sheet
  (`SheetView::entry_locked`), with `SHEET_PROTECTED` in `entry_error`, the
  editor kept with its text, nothing written and no undo step. An untouched
  seeded editor still just closes.
- Unlocked cells stay editable (#2230), as in Excel's form workbooks. A cell
  is locked unless the xf it shows (`entry_style_at`: its own, or an absent
  cell's row/column style, #1660) is `Xf::unlocked`; a missing xf is locked
  (`gridcore` `Sheet::cell_locked`). What writes a value over a range
  (Ctrl+Enter's `commit_edit_to_selection`, Delete's `sheet_clear`, Home ›
  Clear › Clear Contents) needs every cell of every area unlocked
  (`SheetView::areas_locked`, through `Sheet::any_locked`, which never visits
  a whole-sheet area's absent cells one by one). The data-validation list
  (Alt+Down, its arrow, a pick) refuses a locked cell with `SHEET_PROTECTED`
  in the status (`sheet_dv_locked`); before #2230 it wrote into one. The
  entry chords ask only with no editor open: while a formula points, the
  active cell is not the one edited.
- An entry on a protected sheet writes only the cells checked: table
  AutoCorrect (`table_autocorrect`: a table growing to take the entry, a
  calculated column filling its other cells) never runs there, for the
  typed commit and for a validation alert's accepted entry alike. A pick
  from the validation list into an absent cell (`gridcore` `pick_cell`)
  takes the style its row or column gives it (#1660), so a cell of an
  unlocked column stays unlocked after the pick.
- Find & Replace follows the same rule (`replace_refused`): Replace writes
  the matching cell only when it is unlocked, and Replace All (the active
  sheet's cells) is refused whole, nothing replaced, when any cell it would
  change is locked; `SHEET_PROTECTED` in the status either way.
- Every other command still refuses a protected sheet whole: paste, fill,
  drag, Flash Fill, AutoSum, Convert to Number, Copy Formula from Above,
  Format Cells, the Font / Font Size boxes and the other Clear commands.
  The ribbon's and the shortcuts' format commands (Bold, Italic, alignment,
  number formats, colours, borders: `sheet_format_rc` → `apply_format`) do
  not check sheet protection yet. The `<sheetProtection>` flags
  (`formatCells`, `selectLockedCells`, ...) are kept but not read.
- The suite cannot unlock a cell (no Format Cells › Protection tab): unlocked
  xfs come from files. `uiharness/fixtures/protected-form.xlsx`, made by
  `scripts/make-protected-form-fixture.py`, is the form the tests and
  `uiharness/cases/sheet-protected-form.uit` use; the harness reads the
  validation list through `dv-arrow`, `dv-item:N` and the `dv_open` state.
- Review › Protect Sheet is in `act_commits_editor` though not in
  `act_targets_cells` (it takes no selection back from a chart): an open
  entry commits first, as for #510's cell acts, so its Typing step lands
  under the Protect Sheet step; a refused entry stops it, the sheet left
  unprotected. Undo, Redo and Repeat never act on the sheet while editing.

## Floating pictures on the sheet view (#2228)

- `gridcore::drawing::parse_drawings` keeps each anchor whole in
  `Drawing::anchor` (`DrawingAnchor`): its kind, the `colOff`/`rowOff` EMU of
  `from` and `to`, the anchor's own `<xdr:ext>` and `<xdr:pos>` (direct
  children only: a picture's `a:xfrm/a:ext` and an `a:extLst/a:ext uri` are
  not the extent), every EMU clamped to `0..=MAX_ANCHOR_EMU`. An
  `absoluteAnchor` makes a drawing only for a picture (a cell estimate of its
  `pos` for consumers that read cells); an absolute chart stays unread, as
  before. A `blip` inside `xdr:grpSp` makes no picture (one member stretched
  over the group's box); a chart in a group keeps its card.
- Save rewrites only anchors whose `(from, to)` differ from
  `DrawingAnchor::loaded`, so an unedited drawing part keeps its bytes (a
  padded `<xdr:row>`, a clamped column); `loaded: None` (`add_chart`'s) is
  always written. The media and the rels were never touched.
- The suite draws every `DrawingKind::Image` in `sheet_picture_layer`
  (main.rs), placed by `sheet_pictures` in two steps: the anchor becomes
  `(cell, fraction)` edges in Excel's cell sizes (`sheet_row_emu` and
  `sheet_col_emu` over `print::paginate::row_points` / `col_points`, the
  print layout's ECMA-376 measure: a column's own `<col width>` else the
  sheet's default, 9.140625 is 64 px; hidden cells 0; a non-finite or
  negative size is 0 and a huge one stops at `MAX_ANCHOR_EMU`, so the walks
  cannot overflow),
  then drawn px over the grid's sizes: a row at the height the row list
  measured it (a row grows past `row_height_px`, its min height with the
  gridline inside, with its text), else at `row_height_px`, 0 when hidden; a
  column at `col_px`, a hidden one at its width, as the grid draws it. The
  chart cards' `+ 1` per row is not what the grid draws (it drifts a pixel a
  row). The grid is not at Excel's scale (rows 1.4 px/pt, `col_px` clamped),
  so a picture covers the cells it covers in Excel rather than Excel's pixels.
  Walks stop at `MAX_WALK` cells and at the drawn rows/columns.
- Panes follow the grid's own split: the frozen band is `visible[..fr]`, the
  frozen columns `0..fc`, the scrolled pane starts at the list's top item and
  `col0`; each pane clips its pieces, so a picture over a freeze line is drawn
  in each pane it reaches. The layer starts past the row gutter and sits under
  the chart layer; it, the chart layer and the vertical scrollbar start at
  `rows_viewport_top(fx_expanded)`, under the formula bar as drawn. The row
  list and the horizontal scroll reach the drawings too
  (`SheetView::list_rows` and `hscroll_max`, over `view_extent`, capped at
  `DRAWING_REACH_ROWS` rows); `extent` stays the used range for find, Ctrl+A
  and the rest. There is no sheet zoom, so the geometry takes none. The
  frozen band's height is the one the last frame laid out (a canvas on the
  frozen rows, `SheetView::frozen_band`, used while its `fr` matches:
  `frozen_band_px`), so a wrapped frozen header moves the scrolled pane down.
  The rows inside the band and above the list's top item are not measured
  (`ListState::bounds_for_item` has none for them), so a tall text row there
  is taken at its min height.
- The spans are computed once a frame (`sheet_pictures::sheet_spans`) for the
  layer and for the fill handle, which a picture hides when the selection's
  corner lies strictly inside its span (`covers_corner`), over Excel's sizes
  rather than gridcore's estimated `to` cells.
- `pictures::Cache::sheet_image` keys a sheet's media by part: the same bytes
  (address and length) reuse the image, moved bytes are compared with the
  image's, changed bytes make a new one and retire the old. Media gpui can't
  draw (EMF, SVG, TIFF, a corrupt part) is a bordered `<name> (picture)` box.
- `Docxy::picture_sel` selects only (frame and inert grips; no move, resize,
  delete). It holds the selection as a chart does (`sel_hidden`, no fill
  handle); `chart_hand_back` and `chart_drop_selection` clear it, so every
  key and press that hands a chart back, a sheet or tab switch and an undo do
  too. Esc drops it; Delete and Backspace do nothing. A left press it takes
  is held (`picture_held`, set only once the press is taken, ended by
  `grid_release` and by any grid press) so the covered cells' moves do
  not sweep (`sheet_drag_over` stands down, as for a chart drag); a
  right-press selects it with no menu. The pieces keep the default hitbox,
  so the wheel and the root's move and release handlers still see the
  pointer. The harness reports
  `picture_sel` and `sheet_scroll` (the row list's top item), and
  `picture:N` is the piece in the scrolled cells.
- Fixture: `uiharness/fixtures/pictures.xlsx` (`build_pictures` in
  `uiharness/tests/fixture.rs`), case `uiharness/cases/sheet-pictures.uit`.
  xlsxy still draws pictures with `Resize::Fit` (aspect kept).

## Text that spills over empty neighbours (#2240)

- The rules are `gridcore::overflow`'s, shared by the sheet PDF
  (`print/pdf.rs` `draw_page`) and the suite's view (`sheet_spill.rs`).
  Spills: `CellValue::Text`, or a formula shown as text (#676, runs right).
  Never a number (`####` as before), a boolean, an error, a wrapped cell or
  a cell in a merge. General/Left run right, Right left, Center both ways,
  half the excess per side, so a blocked side cuts the text without
  re-centring it. Blockers: any non-blank cell (a `""` formula result
  included) and any merged cell; a styled blank is crossed, and so is a
  hidden column. `Xf` models no indent or shrink-to-fit, so neither counts.
- `spill_span` grows a slot at a time over a width closure: its work is the
  text's width, never the distance to the pane. The PDF passes page slots
  (title columns chained with the page's). A merge's top-left cell prints
  across the merge's slots on the page; the PDF draws no wrap, so a wrapped
  long text is cut at its cell.
- Freeze panes: each pane is its own window onto the sheet, as measured in
  Excel by a third-party viewer
  (yukiyokotani/office-open-xml-viewer#1973, PR #1978). `pane_spills` takes
  the pane's cells plus `prev_blocker`/`next_blocker` around it, computes the
  run in sheet columns and the row clips it to the pane. So a frozen text
  joins the scrolled columns seamlessly unscrolled, a right-aligned text past
  the split runs back into the frozen pane, and a source scrolled off still
  shows its part. An ASCII source that surely fits, or is too far off to
  reach, is not shaped (14.4 px a character bounds a 12 px ASCII glyph);
  any other text is shaped first, since an emoji or U+FDFD has no such bound.
- Drawing (`sheet_row`): the source keeps its text child, `invisible()`, so
  its row keeps the line's height (24 px; a blank row is 21 px, and without
  the child the row shrank and flipped as the editor opened); one absolute layer
  per run, after the row's cells, holds the text in the source cell's own
  box and padding (4 px left, 5 right, 2 top, 3 bottom), aligned in it and
  overflowing to its side, clipped to the run. It has no listeners, so a
  press reaches the covered cell. Cells inside a run drop their right
  gridline. A selected cell in a run gets its ring again in the layer, above
  the glyphs; the layer stays in the row's paint order, under charts,
  pictures and the deferred editor. Not redrawn above the text: a covered
  cell's comment corner and error triangle.
- Widths: `Measurer::new(window)`, built in `render`, at 12 px with the
  cell's bold/italic (conditional formatting included); a line feed in an
  unwrapped text measures its widest line (gpui's `shape_line` refuses a
  `\n`). The edited cell blocks every run and spills nothing, so the in-cell
  editor (and edit-in-cell off's plain entry, #672) covers the text.
- The harness's `spill` key lists the runs the last frame drew (`A8:E8`, a
  run in both panes once) and `sel_row_px` the active row's laid-out
  height; case `uiharness/cases/sheet-spill.uit`.

## Row and column inserts that would push data off the sheet (#1175)

- `gridcore::edit::insert_rows`/`insert_cols` drop whatever they push past
  the last row or column, and they do not check for it. Each host checks
  first, with `edit::insert_shifts_data_off` (a cell that isn't blank, or a
  merged range, at the edge), and refuses with Excel's `SHIFT_OFF_SHEET`
  before an undo step is pushed. The suite (`structural_edit`, which also
  checks notes with `insert_shifts_comments_off`), the xlsxy TUI
  (`row_op`/`col_op`, its status line), the xlsxy control verbs
  `row.insert`/`col.insert` and the MCP tools on them (`"{verb}: …"`), the
  gridwasm dispatch `insrow`/`inscol` (`err`, shown as `view.err`), and its
  `row.insert`/`col.insert` verbs, which read that `err` back as
  `"{verb}: …"`. A new insert entry point needs the same guard. A delete
  never shifts anything off.
- Subtotal inserts whole rows too (`gridcore::edit::subtotal`): once the
  groups are known it checks `groups + 1` rows (a total per group and the
  grand total) at the region's first data row and refuses with
  `SubtotalError::ShiftOffSheet`, whose text is `SHIFT_OFF_SHEET`. With
  Replace the old totals are already gone then (the check is exact, as
  rows come out before rows go in), so both hosts put the workbook back on
  any refusal: the suite's `apply_subtotal` (`restore`) and xlsxy's
  `try_outline_edit_on` (`put_back`).
- The issue's other item (deleting every row of a table left a stale table)
  was already fixed by #991's `delete_table_rows` in `gridcore/src/edit.rs`.
