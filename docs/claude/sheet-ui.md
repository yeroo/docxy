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

## Sheet names Excel refuses on rename (#2115)

- `gridcore::sheet::sheet_name_error(name)` is the one rule, with the reason a
  host shows: empty, over `SHEET_NAME_MAX` (31) characters, any of
  `SHEET_NAME_FORBIDDEN` (`[ ] : * ? / \`), or an apostrophe first or last
  (`Bob's Data` is fine). A taken name is the caller's check.
  `legacy::valid_sheet_names` cleans imported names by the same constants.
- `XlsxPackage::rename_sheet` declines such a name (so `close::commit_rename_buffer`
  and its exit path change nothing). The suite's Enter on the rename bar with a
  changed, refused name sets the status line to the reason and closes the bar;
  Excel keeps the bar open behind a message box (a known deviation).
- xlsxy's `App::rename_sheet` returns the reason as `Err`, which the Rename
  sheet prompt and the `sheet.rename` control show.
- Add paths do not check yet: `XlsxPackage::add_sheet`, xlsxy's AddSheet
  prompt and `pivot.create`'s sheet name, gridwasm's `rename` message.

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
