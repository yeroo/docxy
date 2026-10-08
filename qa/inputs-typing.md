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
