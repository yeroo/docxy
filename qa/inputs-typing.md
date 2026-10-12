# Typing into every editable input — the rule and its manual cases

**Guards:** #1029 (no test typed into a dialog field the way a person does, so
#1027's field that ignored the keyboard passed every test).

## The rule

Every editable input in the suite is typed into with real keys by a case:

- **Dialog fields** are in the catalogue,
  `suite/docxy/src/dialog/catalog/entries.rs`, and the generated
  `uiharness/cases/inputs-typing-*.uit` type into each one with `real-key` and
  `real-type`, focusing it by a real click and by Tab. Every dialog carries a
  catalogued id (a `DialogId` only exists there); a new dialog that reuses
  another's id is caught by review, and by `dialog-catalog-check` where a case
  opens it. A field added to a dialog without its entry fails that dialog's
  `matches the catalogue` case.
- **Adding or changing a dialog:** add or update its entry (how a person opens
  it, every editable control with a sample and what OK shows), run
  `UPDATE_INPUTS_TYPING=1 cargo test --manifest-path suite/Cargo.toml
  inputs_typing_case_is_current`, and commit the regenerated cases. A field the
  cases cannot type into says why (`skip`), and one whose OK they cannot check
  says why (`no_ok`); review those reasons like code.
- **Inputs outside dialogs** (the formula bar, Find, the ribbon's font boxes,
  the comment editor, Backstage's fields, Project's grid, the terminal editors'
  prompts) are listed in `suite/docxy/src/inputs.rs`, each with the issue that
  types into it (#1200 sheet, #1201 document, #1202 Backstage, #1203 Project,
  #1204 terminal). Nothing finds a new one automatically: add it there in the
  change that adds it.
- **The sheet's inputs** (#1200) are typed into by the hand-written
  `uiharness/cases/sheet-inputs-typing.uit` (not `inputs-typing-*`, which the
  catalogue owns): the formula bar, the in-cell editor, the Name Box, the Find
  bar's Find and Replace fields, and Home's Font and Font Size boxes. Each is
  focused by `pointer-click {"input":id}` (the in-cell editor by a double-click
  on the cell) and read by `field-read {"input":id}`; each test opens its own
  copy and checks one step against what Excel does. The Find bar's Replace and
  All buttons have no key, so those steps click them for real.
- **The document's inputs** (#1201) are typed into by the hand-written
  `uiharness/cases/doc-inputs-typing.uit` the same way: the Find bar's Find
  and Replace fields, Home's Font and Font Size boxes, and the comment editor,
  which Review > New Comment on a selection opens focused (a click on it does
  nothing more). Each test opens its own copy of `doc-table.docx` and checks
  one step against what Word does, except that Enter in the Replace field
  replaces, as the bar says. The suite has no Styles gallery search to type
  into.
- **A field broken today** is listed in `uiharness/cases/expected-failures.txt`
  with its own issue, per failing step, so the sweep stays green and any new
  failure fails CI. Remove the entry in the change that fixes it.

`python3 scripts/inputs-typing-table.py <sweep run dir>` prints the sweep's
result as a table of surface, dialog, field, step and result.

## Manual cases

What the harness cannot reach: the OS's own keyboard and a person's mouse. Run
these once on each OS after a change to dialog key handling. Use a scratch
config (`DOCXY_CONFIG_DIR` or a fresh profile), never your own.

**Setup:** `cargo build -p docxy`, run the suite, open `assets/sample.docx` and
a workbook.

### A number field

1. Layout › Margins › Custom Margins…. Click into Top.
2. Ctrl+A, type `1.5`; type a letter; Backspace; Home, Right, Delete; End,
   Left, type `9`; Ctrl+A, Ctrl+C, then paste it into Bottom with Ctrl+A,
   Ctrl+V.
3. Press Enter; reopen Custom Margins….

**Expect:** the letter is refused (the status says the field takes a number)
and every other key edits at the caret; Ctrl+C puts the selection on the
clipboard; Enter applies and the dialog reopens on the typed margins.

### A tabbed dialog and Escape

1. In the workbook, Page Layout › Print Titles. Click the Margins tab, click
   Top, type `2`; click the Sheet tab, type into Rows to repeat at top.
2. Press Escape; reopen.

**Expect:** each tab's fields take the keys; Escape discards everything typed.

**Fails when:** a dialog field ignores typed keys, the clipboard chords, or
Enter and Escape, from any of the paths above.
