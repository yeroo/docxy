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
