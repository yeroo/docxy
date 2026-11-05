# Terminal editors

Notes moved from `CLAUDE.md` (#1903). Read them before changing the terminal editors (`docxy`, `xlsxy`, `yppxy`): the Layout tab and line editing in their prompts and in `backstagecore`'s Save As name. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Terminal docxy Layout tab (#1274)

- `docxy/src/layout_tab.rs` is terminal `docxy`'s Layout ribbon tab (after
  Design): Margins, Orientation, Size, Columns, Breaks, Line Numbers and
  Hyphenation are `PickerKind` list pickers (dialog-only rows of the suite's
  menus are absent). Insert has no Layout group. The presets duplicate
  `suite/docxy/src/layout_tab.rs`; keep their values the same.
- Section picks edit `body_editor_mut().target_sections()` through
  `edit_section_setups` (the section(s) at the caret or selection, even with
  a header open), then `sync_page_parts` and `after_edit`. A picker starts on
  the caret section's current choice. Mirrored margins and Hyphenation write
  the package only (a settings part docxy cannot edit refuses) and have no
  undo in the terminal.

## Terminal docxy prompts edit at a caret (#1372)

- Find, Replace, the comment bar, Compare's two paths and the vim `:` line are
  `docxy/src/line_edit.rs`'s `LineEdit` (text plus a char caret, shared key set
  in `handle`): Left, Right, Home, End, Delete and Backspace act at the caret,
  with no Ctrl or Alt (`LineEdit::is_edit_key`, so Ctrl chords keep their
  meaning), and typing inserts there (each prompt's Char arm keeps its own
  modifier rule). Only the
  focused field draws the `▏` mark, at its caret; `replace_drawn` feeds both
  the Replace status text and the `docxy-replace` `at`, and `compare_window`
  keeps a long Compare path's caret in view. `field-read` reports the real
  caret (`Field::with_caret`). `LineEdit` also holds a selection
  (`select_all`, `selection`, `delete_selection`; typing, Backspace and
  Delete replace or remove it, the arrows, Home and End drop it) and
  `field-read` reports it. In Find and Replace (#1375) Ctrl+A selects the
  focused field and Replace All is Alt+A (nothing in find-only mode). The
  comment bar and Compare's two paths take Ctrl+A too (#1376); the vim `:`
  line does not. Find, Replace and Compare copy and paste (#1377, below), and
  so does the comment bar (#1378, below); the vim `:` line and
  click-to-place are still not there. A click on the Find bar's Find or
  Replace box focuses it (#1379): `replace_hit` records the status row and the
  cells of the two boxes as drawn (the label, the ` → ` and the cells after
  the Replace box focus nothing); the click only switches `editing_replacement`
  and does not place the caret, and a drag or release on that row is consumed.

## Terminal editors: Ctrl+C/X/V in Find, Replace, Compare, the minibuffer and the cell editor (#1377)

- Ctrl+C copies the field's selection, Ctrl+X cuts it, Ctrl+V pastes the first
  line of the clipboard (control characters dropped) at the caret, over a
  selection. With no selection Ctrl+C and Ctrl+X do nothing, and none types a
  letter. An empty paste (the clipboard starts with a line break or holds only
  control characters) changes nothing in every editor, a selection included. The text comes from the OS clipboard, else from `clip_text` (the last
  text the editor put there: a headless Linux has no `os_clip`, which is the
  harness's path). docxy's `LineEdit` has `selected_text`, `cut`, `paste`;
  terminal docxy's `App::field_copy`/`field_paste_text` serve Find (a paste or cut in the
  query re-runs the match), Replace and Compare. xlsxy's `Prompt` has
  `selected_text`/`paste`, `App::field_copy`/`field_paste_line`; without an OS
  clipboard a grid paste after a field copy falls back to `clip_text`.
- A field copy replaces the other clip with the field's plain text: docxy sets
  `clipboard` to `Clip::from_text`; xlsxy clears `clip`, so a grid paste of that
  text is external TSV and not the old cells (`paste_from` treats an OS text
  equal to `clip_text` as its own clip).
- The xlsxy cell editor has only a select-all selection, so Ctrl+C/X take the
  text only while it is all selected (Ctrl+C keeps the selection); Ctrl+V
  inserts at the caret, replacing the text under select-all; a paste is not
  typing (`typed_tail` false, so no AutoCorrect at commit, and no AutoComplete
  proposal), and an empty paste changes nothing.

## Terminal editors: a Ctrl chord types nothing; copy and paste in the comment bar, Save As name and yppxy's prompt (#1378)

- Ctrl+C/X/V (no Alt) copy, cut and paste as in #1377 in docxy's comment
  bar (`comment_field_clip`), Backstage Save As's file name in all three
  editors, and yppxy's `Prompt` (every prompt, the vim `:` line included). Any
  other Ctrl chord without Alt types nothing there; Ctrl+Alt (AltGr) still
  types. docxy's and xlsxy's vim `:` lines are unchanged.
- `backstagecore` stays clipboard-free: `Backstage::name_has_keyboard`,
  `name_selected_text`, `name_cut`, `name_paste` and `backstagecore::first_line`
  (the one first-line, controls-dropped rule) serve the host, whose
  `backstage_name_clip` runs before `backstage_key` takes the backstage and
  passes every other key on. docxy and yppxy use `field_copy` /
  `field_paste_text`; xlsxy `field_copy` / `field_paste_line`.
- yppxy now has a clipboard (`arboard`, `os_clip`, else `clip_text`), and Ctrl+C
  on the grid copies the selected task's name as plain text. There is no task
  paste.

## Terminal editors: Ctrl+A in the other prompts (#1376)

- Ctrl+A selects the whole field, the caret at its end, and typing, Backspace
  and Delete replace or remove it; Left/Right collapse it to an edge and
  Home/End drop it; `field-read` reports `selection`. An empty field selects
  nothing, and the key never types an `a`. Each editor keeps its own small
  anchor (no shared line-edit crate): docxy's comment bar and Compare use
  `LineEdit`; xlsxy's minibuffer `Prompt` and yppxy's `Prompt` have
  `anchor`/`selection`/`delete_selection`; the cell editor reuses
  `EditState.select_all` (Ctrl+A sets it, Left collapses to 0; the
  end-of-branch reset keeps it for that key, and Ctrl+A skips the tail that
  ends a correction and refreshes the formula list). Left/Right collapse
  only when editing in place (F2); in type-over mode they still commit and
  move, as in Excel. Backstage Save As's name, shared by the
  three, has `Backstage::name_anchor`, drawn reversed and reported only while
  the name is focused (Tab away hides it, Tab back shows it again); every
  write of `name_cursor` clears it. yppxy's vim `:` line is a `Prompt`, so it
  takes Ctrl+A. The xlsxy vim `:` line has no select-all.

## xlsxy terminal vim `:` line edits at a caret (#1373)

- `VimState::cmdline` is `xlsxy/src/main.rs`'s `CmdLine` (text plus a char
  caret): Left, Right, Home, End, Delete and Backspace act at the caret, with
  no Ctrl or Alt (`vim_key` passes `ctrl || alt`), and typing inserts there
  (any modifier, as before). The hint line draws a reversed block at the
  caret, as the minibuffer prompt does, and `field-read xlsxy-vim-command`
  reports the real caret. Copy/paste and click-to-place are not there;
  nor is select-all (#1376 covers the minibuffer prompt, not this line).

## yppxy terminal prompt edits at a caret (#1374)

- `Prompt` in `yppxy/src/main.rs` (Rename, Duration, Predecessor, Constraint,
  Assign, Find, Notes, Save As on a new project and the vim `:` line) is text
  plus a char caret, built through `Prompt::new` (caret at the end of the
  seed). Left, Right, Home, End and Delete act at the caret with no Ctrl or Alt
  (such chords are still swallowed), Backspace and typing act there, and Enter
  commits the whole text wherever the caret is. The edits clamp the caret, as
  tests assign `buf` directly. The drawn `▏` sits at the caret and `field-read
  yppxy-prompt` reports it. Ctrl+A selects the buffer (#1376; typing and the
  edit keys replace or collapse it). Copy/paste is there (#1378, above);
  click-to-place is not; Backstage Save As's name field is separate.
